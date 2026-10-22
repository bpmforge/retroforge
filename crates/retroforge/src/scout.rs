//! Ticket W32-07: the level scout -- a second machine that draws the level
//! AHEAD of (and behind) the camera so the widescreen margins show real level
//! the player has not visited.
//!
//! The stitched canvases ([`rf_enhance::canvas`]) only know what the camera
//! has seen. The scout is the same machine, restored from a snapshot of the
//! live one at the moment a scene is entered, then driven somewhere the
//! player has not been: each scout frame it pokes the game's own camera (or
//! the player's position, so the game's follow logic moves the camera and
//! streams the columns), steps one frame into a private sink and stitches the
//! picture with a [`CanvasModel`] whose world axes continue the live ones
//! (one model for both consoles, W33-03), so scout cells land on the same
//! world coordinates as live ones. Its cells then fill only the UNKNOWN cells
//! of the live canvases ([`CanvasModel::merge_unknown_from`]): live always
//! wins.
//!
//! **Law 4 (the live core is never touched).** The scout owns its own
//! [`EmuStepper`]; the live one is only ever read (`snapshot`). The scout's
//! machine shares no mutable state with the live one, its audio is dropped,
//! and its input is held neutral. Sprites are never taken from it (only
//! `Background(n)` pixels are stitched), because anything the scout's
//! player does to the game -- enemies waking, items spawning -- must not
//! appear in the live picture.
//!
//! **Threads (W32-08).** The scout used to run in slices on the core thread
//! and cost about a quarter of the live fps under load. Now the core thread
//! only DECIDES (when to start, the snapshot of the live machine, merging)
//! and a worker thread ([`ScoutDriver`] spawns one per scout) does the
//! stepping: it opens its own machine from the ROM bytes, restores the
//! snapshot, runs the passes and ships canvas deltas (the dirty boxes) back
//! over a bounded channel. The live core is still only ever READ (one
//! `snapshot`, on the core thread); the scout machine is a separate instance
//! that nothing else can reach, so law 4 (nothing mutates core state off the
//! core thread) concerns the live machine, which no scout code touches. A
//! scene change, a new setup, widescreen off or shutdown cancels the worker
//! (a flag it checks every frame). `Scout::step` still runs one frame and
//! returns, so tests drive a `Scout` directly without a thread.
//!
//! **Self-check and ladder (W33-06).** The worker does not trust its own
//! cells. Before a delta leaves the worker it is compared with a copy of the
//! live canvas taken when the scout started ([`ScoutReference`]): >= 99 %
//! agreement over >= 2 000 shared cells merges, 95-99 % merges and lowers the
//! step for the next pass, less is discarded and the pass is retried at half
//! the step (down to a floor), then on the next rung of the strategy ladder
//! (`crate::scout_ladder`), and a scene no rung can scout is given up. A
//! delta the live canvas cannot judge yet (too few shared cells) is held until
//! one can; a pass that never gets a verdict fails its rung. The first judged
//! delta decides the pass (it is the one that overlaps the screen the player
//! stands on); a later delta judged "reject" is only dropped, because where
//! it meets the live canvas again that is its newest edge, which may be
//! half drawn. Because the live
//! canvas only holds the screen the player stands on and a too-fast scout is
//! wrong in the columns AHEAD of it (SMB3 at 8 px per frame), a step is first
//! checked by a probe: the same stretch scouted at half the step, compared
//! cell by cell. The merge tags its cells `Scout` ([`rf_enhance::stitcher`]),
//! a live frame turns them `Live` again, and a scout never writes over a live
//! cell.
//!
//! The recipe (which RAM words, how far per frame) is per game and comes from
//! the profile's `[scout]` table; the pure rules (what to poke, when to stop)
//! are plain functions so they are tested without a ROM.

use rf_core_api::{CoreSink, EventMask, FrameBundleBuilder, InputFrame, PpuPixel};
use rf_enhance::canvas::{ScoutReference, ScoutVerdict, MIN_SHARED_CELLS};
use rf_enhance::scene_identity::SceneId;

use crate::scout_ladder::{
    check_delta, rung_of_code, rungs_for, Attempt, DeltaCheck, Ladder, PassReport, ScoutReport,
};

use rf_enhance::stitcher::Canvas;

use crate::stepper::{EmuStepper, MachineSnapshot};
use rf_enhance::canvas::{
    scrolls_for, CanvasModel, FrameFeed, FrameScrolls, LayerFrames, LayerScroll, ScrollLag, LAYERS,
};

/// How the scout moves the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoutStrategy {
    /// Write the camera X words directly each frame.
    CameraWrite,
    /// Move the player X words; the game's own follow logic moves the camera
    /// and streams the level.
    PlayerMove,
    /// Ticket W32-09 (NES): poke nothing of the player's; the scout's pad
    /// holds the pass's direction, because the camera of some games (Mega Man
    /// 2) only follows a player who walks.
    Walk,
}

/// Ticket W33-08: the axis a pass scouts along. A scene scrolls along one
/// (the scene tracker's axis rule), and the scout follows it: `X` passes walk
/// right and left, `Y` passes down and up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoutAxis {
    X,
    Y,
}

impl ScoutAxis {
    /// The passes a scene of this axis gets when the profile names none on it:
    /// forward first (right, down), then back.
    #[must_use]
    pub fn default_dirs(self) -> Vec<ScoutDir> {
        match self {
            ScoutAxis::X => vec![ScoutDir::Right, ScoutDir::Left],
            ScoutAxis::Y => vec![ScoutDir::Down, ScoutDir::Up],
        }
    }
}

/// Which way a pass walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoutDir {
    Right,
    Left,
    Up,
    Down,
}

impl ScoutDir {
    /// The axis this direction moves along.
    #[must_use]
    pub const fn axis(self) -> ScoutAxis {
        match self {
            ScoutDir::Right | ScoutDir::Left => ScoutAxis::X,
            ScoutDir::Up | ScoutDir::Down => ScoutAxis::Y,
        }
    }

    /// Screen coordinates grow right and DOWN.
    const fn sign(self) -> i32 {
        match self {
            ScoutDir::Right | ScoutDir::Down => 1,
            ScoutDir::Left | ScoutDir::Up => -1,
        }
    }

    /// The NES pad bit that walks this way (nesdev "Controller reading":
    /// A B Select Start Up Down Left Right = bits 0..7).
    const fn nes_pad_bit(self) -> u8 {
        match self {
            ScoutDir::Up => 1 << 4,
            ScoutDir::Down => 1 << 5,
            ScoutDir::Left => 1 << 6,
            ScoutDir::Right => 1 << 7,
        }
    }

    /// The profile's word for it.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "right" => Some(ScoutDir::Right),
            "left" => Some(ScoutDir::Left),
            "up" => Some(ScoutDir::Up),
            "down" => Some(ScoutDir::Down),
            _ => None,
        }
    }
}

/// A game's scout recipe (from the profile's `[scout]` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoutPlan {
    pub strategy: ScoutStrategy,
    /// Bus addresses of 16-bit little-endian camera X words (the first is
    /// read; all are written by [`ScoutStrategy::CameraWrite`]).
    pub camera: Vec<u32>,
    /// Bus addresses of 16-bit little-endian player X words (the first is
    /// read; all are written by [`ScoutStrategy::PlayerMove`]).
    pub player_x: Vec<u32>,
    /// Bus addresses of the HIGH bytes of the player X words, one per
    /// `player_x` address, for a game that keeps the two bytes apart (the NES
    /// games of W32-09). Empty: the high byte is the next address.
    pub player_x_hi: Vec<u32>,
    /// Ticket W33-08: the same for a VERTICAL scene. `camera_y` words are 16-bit
    /// little-endian; a `player_y` word with a `player_y_hi` is 16-bit, one
    /// with none is a single byte (Mega Man 2 keeps Y in one).
    pub camera_y: Vec<u32>,
    pub player_y: Vec<u32>,
    pub player_y_hi: Vec<u32>,
    /// Pixels per scout frame.
    pub step_px: u16,
    /// Most frames one pass may run.
    pub frame_budget: u32,
    /// A pass ends when the canvases have not grown for this many frames.
    pub stall_frames: u32,
    /// Live frames to wait after a scene is entered before scouting it (the
    /// game is still loading / fading in before that).
    pub settle_frames: u32,
    /// The passes, in order. Each restarts from the snapshot.
    pub dirs: Vec<ScoutDir>,
    /// Constant `(bus address, value)` pokes applied every frame (a "safe"
    /// table: invincibility, a frozen timer), only when found by observation.
    pub hold: Vec<(u32, u8)>,
    /// `(bus address, value)` conditions on the LIVE machine that must all
    /// hold for a scout to start (the player is alive and in control: a
    /// scout started from a dying player only gets as far as the game's
    /// own restart timer). W35-11: rows on one address are alternatives (a
    /// mode byte that reads one value in a field and another while the
    /// screen scrolls); different addresses must all hold ([`start_gate_open`]).
    pub start_when: Vec<(u32, u8)>,
    /// Ticket W33-06: the profile's strategy is final: the ladder tries no
    /// other rung.
    pub pin: bool,
    /// Ticket W33-07: the profile's `[scene]` key, so the scout's own scene
    /// tracker is fed the same level bytes the live one is (without them a
    /// level the profile names is a "key change" on the scout's first frame,
    /// which ends the pass at once). The app sets it from the matched profile.
    pub scene_key: Option<crate::scene_key::SceneKeySpec>,
    /// W35-09: how far past the live picture the margins reach, in pixels each side (derived
    /// from the widescreen aspect by the app: `(output width - source width) / 2`). 0 = not
    /// asked: a pass ends on its stall rule only. A walk that is stalled with a margin still
    /// unfilled sidesteps (see [`sidestep_pad`]) before it gives up.
    pub reach_px: u32,
}

/// No addresses (the walk strategy pokes none).
static NO_ADDRS: Vec<u32> = Vec::new();

impl ScoutPlan {
    /// Build the plan from a (validated) profile `[scout]` table.
    #[must_use]
    pub fn from_profile(t: &rf_profiles::schema::Scout) -> Self {
        let words = |role: &str| -> Vec<u32> {
            t.addr
                .iter()
                .filter(|r| r.role == role)
                .map(|r| r.addr)
                .collect()
        };
        let valued = |role: &str| -> Vec<(u32, u8)> {
            t.addr
                .iter()
                .filter(|r| r.role == role)
                .filter_map(|r| r.value.map(|v| (r.addr, v)))
                .collect()
        };
        let dirs = t.dirs.as_ref().map_or_else(
            || ScoutAxis::X.default_dirs(),
            |d| {
                d.iter()
                    .map(|s| ScoutDir::parse(s).unwrap_or(ScoutDir::Right))
                    .collect()
            },
        );
        Self {
            strategy: match t.strategy.as_str() {
                "camera_write" => ScoutStrategy::CameraWrite,
                "walk" => ScoutStrategy::Walk,
                _ => ScoutStrategy::PlayerMove,
            },
            camera: words("camera_x"),
            player_x: words("player_x"),
            player_x_hi: words("player_x_hi"),
            camera_y: words("camera_y"),
            player_y: words("player_y"),
            player_y_hi: words("player_y_hi"),
            step_px: t.step_px,
            frame_budget: t.frame_budget,
            stall_frames: t.stall_frames.unwrap_or(120),
            settle_frames: t.settle_frames.unwrap_or(90),
            dirs,
            hold: valued("hold"),
            start_when: valued("start_when"),
            pin: t.pin.unwrap_or(false),
            scene_key: None,
            reach_px: 0,
        }
    }

    /// The camera words of `axis`.
    #[must_use]
    pub fn camera_words(&self, axis: ScoutAxis) -> &[u32] {
        match axis {
            ScoutAxis::X => &self.camera,
            ScoutAxis::Y => &self.camera_y,
        }
    }

    /// The player words of `axis`.
    #[must_use]
    pub fn player_words(&self, axis: ScoutAxis) -> &[u32] {
        match axis {
            ScoutAxis::X => &self.player_x,
            ScoutAxis::Y => &self.player_y,
        }
    }

    /// The passes a scene scrolling along `axis` gets: the profile's
    /// directions on that axis, else the axis's defaults.
    #[must_use]
    pub fn dirs_for(&self, axis: ScoutAxis) -> Vec<ScoutDir> {
        let own: Vec<ScoutDir> = self
            .dirs
            .iter()
            .copied()
            .filter(|d| d.axis() == axis)
            .collect();
        if own.is_empty() {
            axis.default_dirs()
        } else {
            own
        }
    }

    /// Whether this recipe can move the scout along `axis` on a machine
    /// placed by `bands` (the NES: `walk` holds the pad): some word to poke,
    /// or a pad to hold. A scene whose axis it cannot move is scouted along
    /// the other one, as before W33-08, rather than given up.
    #[must_use]
    pub fn can_scout(&self, axis: ScoutAxis, bands: bool) -> bool {
        bands || !self.camera_words(axis).is_empty() || !self.player_words(axis).is_empty()
    }

    /// The player's word on `axis` as the scout's machine holds it: the first
    /// `player_x` address, with its high byte at the matching `player_x_hi`
    /// address (else the next one); a `player_y` with no `player_y_hi` is one
    /// byte.
    fn player_word(&self, st: &EmuStepper, axis: ScoutAxis) -> u16 {
        let (words, his) = match axis {
            ScoutAxis::X => (&self.player_x, &self.player_x_hi),
            ScoutAxis::Y => (&self.player_y, &self.player_y_hi),
        };
        words.first().map_or(0, |&a| {
            let lo = u16::from(st.peek_bus(a));
            match (axis, his.first()) {
                (ScoutAxis::Y, None) => lo,
                (_, hi) => {
                    let hi = hi.copied().unwrap_or(a + 1);
                    lo | (u16::from(st.peek_bus(hi)) << 8)
                }
            }
        })
    }

    /// Whether `addr` is a byte of the player's Y word: a hold on it keeps
    /// the player where the X recipe wants him and fights a Y one.
    fn is_y_hold(&self, addr: u32) -> bool {
        self.player_y.contains(&addr) || self.player_y_hi.contains(&addr)
    }

    /// The camera moved by more than this in one frame is a scene change
    /// (a transition or a respawn), not scrolling.
    pub const JUMP_PX: i32 = 64;

    /// Where a poke of `dir` puts a word that is at `from`: a step along the
    /// direction, held inside the word's range (a Y byte is 8-bit).
    #[must_use]
    pub fn moved(&self, dir: ScoutDir, from: u16) -> u16 {
        let top = if dir.axis() == ScoutAxis::Y
            && self.strategy == ScoutStrategy::PlayerMove
            && self.player_y_hi.is_empty()
        {
            i32::from(u8::MAX)
        } else {
            i32::from(u16::MAX)
        };
        (i32::from(from) + i32::from(self.step_px) * dir.sign()).clamp(0, top) as u16
    }

    /// The pokes for one scout frame, given what the scout machine holds now
    /// (`camera` and `player` on the axis of `dir`).
    ///
    /// On a Y pass the holds on the player's Y word are RELEASED: they pin him
    /// above the obstacles of a horizontal level, and a Y recipe moves that
    /// very word (Super Mario World's camera Y stays at its floor with Mario
    /// held above the screen, W32-07).
    #[must_use]
    pub fn pokes(&self, dir: ScoutDir, camera: u16, player: u16) -> Vec<(u32, u8)> {
        let axis = dir.axis();
        let (addrs, from) = match self.strategy {
            ScoutStrategy::CameraWrite => (self.camera_words(axis), camera),
            ScoutStrategy::PlayerMove => (self.player_words(axis), player),
            ScoutStrategy::Walk => (&NO_ADDRS[..], 0),
        };
        let to = self.moved(dir, from);
        let mut out: Vec<(u32, u8)> = Vec::with_capacity(addrs.len() * 2 + self.hold.len());
        for (i, &a) in addrs.iter().enumerate() {
            out.push((a, to as u8));
            let hi = match (self.strategy, axis) {
                (ScoutStrategy::PlayerMove, ScoutAxis::X) => {
                    Some(self.player_x_hi.get(i).copied().unwrap_or(a + 1))
                }
                (ScoutStrategy::PlayerMove, ScoutAxis::Y) => self.player_y_hi.get(i).copied(),
                (ScoutStrategy::CameraWrite | ScoutStrategy::Walk, _) => Some(a + 1),
            };
            if let Some(hi) = hi {
                out.push((hi, (to >> 8) as u8));
            }
        }
        out.extend(
            self.hold
                .iter()
                .filter(|(a, _)| axis == ScoutAxis::X || !self.is_y_hold(*a))
                .copied(),
        );
        out
    }
}

/// Why a pass (or the whole scout) ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoutEnd {
    /// The frame budget ran out.
    Budget,
    /// No new level for [`ScoutPlan::stall_frames`] frames (the camera hit
    /// its end, or the game refused to follow).
    Stalled,
    /// The camera jumped: the scout machine left the scene.
    SceneChanged,
}

/// The NES picture's width: its frame stays this wide whatever the output (core_thread).
const NES_SOURCE_WIDTH: i64 = 256;

/// Sidesteps per stuck spell; sidestep `k` goes sideways for `SIDESTEP_UNIT * (k + 1)` frames
/// (alternately up and down: a zigzag of growing reach), then forward for [`SIDESTEP_FORWARD`].
pub const SIDESTEPS: u32 = 8;
const SIDESTEP_UNIT: u32 = 8;
/// Frames of forward walking after each sidestep.
pub const SIDESTEP_FORWARD: u32 = 160;

/// W35-09: the pad bits a blocked walker presses `stuck` frames after it first stood still, or
/// `None` once the zigzag is spent. A walk that holds only its direction stops at the first
/// tree, rock or wall of a top-down screen; a game that moves its camera by flips (the Legend of
/// Zelda) shows nothing beyond until the player gets past. The walker goes sideways (up, then
/// down, then up further...) and tries the way forward again after each. Only `Up`/`Down` are
/// pressed: a game that needs another sidestep key gets none. Total frames
/// `sum(SIDESTEP_UNIT * (k + 1) + SIDESTEP_FORWARD)`, bounded: the zigzag is spent after that.
#[must_use]
pub fn sidestep_pad(stuck: u32, dir: ScoutDir) -> Option<u8> {
    let mut at = 0;
    // Bounded: SIDESTEPS iterations.
    for k in 0..SIDESTEPS {
        let side = SIDESTEP_UNIT * (k + 1);
        if stuck < at + side {
            let perp = if k % 2 == 0 {
                ScoutDir::Up
            } else {
                ScoutDir::Down
            };
            return Some(perp.nes_pad_bit());
        }
        at += side;
        if stuck < at + SIDESTEP_FORWARD {
            return Some(dir.nes_pad_bit());
        }
        at += SIDESTEP_FORWARD;
    }
    None
}

/// The stop rules, as a small state machine over per-frame facts.
#[derive(Debug, Clone, Copy, Default)]
pub struct StopRule {
    frames: u32,
    still: u32,
    extent: usize,
    last_camera: Option<i64>,
    /// Ticket W33-08: frames a pass may run without growing the canvases
    /// before the stall rule counts (see [`Y_LEAD_IN_FRAMES`]).
    lead_in: u32,
    /// The extent after the pass's first frame (which stitches a screen of
    /// its own), and whether anything has grown past it since.
    first: Option<usize>,
    grew: bool,
    /// W35-09: the walker is sidestepping round an obstacle: the stall rule waits (the pass is
    /// still bounded by its frame budget and by the zigzag's own end).
    defer_stall: bool,
}

/// A vertical pass's lead-in. A game that scrolls down only for a player who
/// reaches the bottom of the screen (Mega Man 2: the poked Y crosses 0xF0
/// after 40 frames at 4 px, and the screen starts to scroll about 30 frames
/// later) has stitched nothing new for far longer than the stall rule's 60.
pub const Y_LEAD_IN_FRAMES: u32 = 300;

impl StopRule {
    /// The rules for a pass in `dir`.
    #[must_use]
    pub fn for_dir(dir: ScoutDir) -> Self {
        Self {
            lead_in: match dir.axis() {
                ScoutAxis::X => 0,
                ScoutAxis::Y => Y_LEAD_IN_FRAMES,
            },
            ..Self::default()
        }
    }

    /// Frames since the canvases last grew.
    #[must_use]
    pub fn still(&self) -> u32 {
        self.still
    }

    /// Whether `camera` is a jump from the last frame's (a scene change).
    #[must_use]
    pub fn jumped(&self, camera: i64) -> bool {
        self.last_camera
            .is_some_and(|prev| (camera - prev).abs() > i64::from(ScoutPlan::JUMP_PX))
    }

    /// Feed one frame: the camera word read after it and the canvases'
    /// total extent (cells in their bounding boxes). `Some` ends the pass.
    pub fn observe(&mut self, plan: &ScoutPlan, camera: i64, extent: usize) -> Option<ScoutEnd> {
        if self.jumped(camera) {
            return Some(ScoutEnd::SceneChanged);
        }
        self.last_camera = Some(camera);
        self.frames += 1;
        if extent > self.extent {
            self.extent = extent;
            self.still = 0;
        } else {
            self.still += 1;
        }
        let first = *self.first.get_or_insert(extent);
        self.grew |= extent > first;
        if self.still >= plan.stall_frames
            && !self.defer_stall
            && (self.grew || self.frames >= self.lead_in)
        {
            Some(ScoutEnd::Stalled)
        } else if self.frames >= plan.frame_budget {
            Some(ScoutEnd::Budget)
        } else {
            None
        }
    }
}

/// What a [`Scout`] is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoutStatus {
    Running,
    /// Every pass is over; the last pass ended for this reason.
    Finished(ScoutEnd),
}

/// Why a scout could not start.
#[derive(Debug)]
pub enum ScoutError {
    Open(String),
    Restore(String),
}

/// Reads a little-endian word at a bus address.
fn word(st: &EmuStepper, addr: u32) -> u16 {
    u16::from(st.peek_bus(addr)) | (u16::from(st.peek_bus(addr + 1)) << 8)
}

/// Collects a frame as a bundle; audio is dropped, and events are kept only
/// for a machine placed by bands (the NES; a SNES is placed by registers).
struct ScoutSink {
    bundle: FrameBundleBuilder,
    /// The NES is placed by the core's own scroll and scanline events; the
    /// SNES reads registers and wants none.
    keep_events: bool,
}

impl CoreSink for ScoutSink {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.bundle.video_scanline(y, pixels);
    }
    fn video_scanline_widened(&mut self, y: u16, pixels: &[PpuPixel], pad: usize) {
        let w = usize::from(self.bundle.width());
        self.bundle
            .video_scanline(y, pixels.get(pad..pad + w).unwrap_or(pixels));
    }
    /// Ticket W33-01: the scout stitches with the same sub-screen and
    /// per-line registers the live path does (its bundle builder captures
    /// them), so a hill the live game only ever drew on the sub screen is
    /// scouted too.
    fn sub_scanline(&mut self, y: u16, pixels: &[rf_core_api::SubPixel], fixed_color: u16) {
        self.bundle.sub_scanline(y, pixels, fixed_color);
    }
    fn wants_line_registers(&self) -> bool {
        self.bundle.wants_line_registers()
    }
    fn line_registers(&mut self, y: u16, regs: &rf_core_api::LineRegisters) {
        self.bundle.line_registers(y, regs);
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, ev: rf_core_api::CoreEvent) {
        if self.keep_events {
            self.bundle.event(ev);
        }
    }
}

/// Reads the per-layer scroll the stitcher needs off the scout's machine
/// (the same function the live path uses, so the two agree on layers).
pub type ScrollFn<'a> = &'a dyn Fn(&EmuStepper) -> Option<[Option<LayerScroll>; LAYERS]>;

pub struct Scout {
    stepper: EmuStepper,
    snapshot: MachineSnapshot,
    plan: ScoutPlan,
    scene: SceneId,
    /// The live model's world axes and trackers at the snapshot (no canvases):
    /// a pass restarts here.
    seed: CanvasModel,
    acc: CanvasModel,
    sink: ScoutSink,
    pass: usize,
    rule: StopRule,
    status: ScoutStatus,
    frames_total: u64,
    since_merge: u32,
    /// The scroll read after the previous frame: what drew the frame being
    /// stitched (the shared [`ScrollLag`] rule, W32-08). SNES only.
    lag: ScrollLag,
    /// The lag holds the restored registers' read (set at a pass start).
    primed: bool,
    /// A NES machine: frames are placed by the core's own scroll events
    /// (bands), not by layer registers (W32-09).
    bands: bool,
    /// World x (y, on a vertical pass) of the last frame's picture: the camera
    /// the stop rules watch on the NES, where the stitcher's own placement is
    /// the only camera.
    camera: Option<i64>,
    /// Ticket W33-08: where the last Y poke put the player's Y word. A Y pass
    /// pokes from THIS, not from what the game holds: a player standing on a
    /// floor is put back on it every frame, and a poke from his own value
    /// would never get past it.
    y_target: Option<u16>,
    /// W35-09: the camera at the pass's first frame (the margins are measured from it).
    camera0: Option<i64>,
    /// How the passes ended, in order.
    pub ends: Vec<ScoutEnd>,
}

impl Scout {
    /// Open a second machine of `rom` and put it where `snapshot` was taken.
    /// `live` is the live model at that moment (its world axes and scene
    /// tracking are copied, no canvases); `size` is the live frame's
    /// `(width, height)`.
    ///
    /// # Errors
    /// The ROM cannot be opened, or the snapshot cannot be restored.
    pub fn start(
        rom: &[u8],
        snapshot: MachineSnapshot,
        plan: ScoutPlan,
        live: &CanvasModel,
        scene: SceneId,
        size: (u16, u16),
    ) -> Result<Self, ScoutError> {
        let mut stepper = EmuStepper::open(rom).map_err(|e| ScoutError::Open(format!("{e:?}")))?;
        stepper
            .restore(&snapshot)
            .map_err(|e| ScoutError::Restore(format!("{e:?}")))?;
        Ok(Self::from_stepper(
            stepper, snapshot, plan, live, scene, size,
        ))
    }

    /// As [`Self::start`] with a machine the caller already restored.
    #[must_use]
    pub fn from_stepper(
        mut stepper: EmuStepper,
        snapshot: MachineSnapshot,
        plan: ScoutPlan,
        live: &CanvasModel,
        scene: SceneId,
        size: (u16, u16),
    ) -> Self {
        // A machine with no background layers is the NES, which places by the
        // core's own scroll events.
        let bands = stepper.bg_layer_views().is_none();
        if bands {
            // The scroll/scanline events the stitcher reads (the stepper
            // always keeps this baseline, whatever the mask asked for).
            stepper.set_event_mask(EventMask::NONE);
        }
        let seed = live.fork_for_scout();
        Self {
            stepper,
            snapshot,
            scene,
            acc: seed.fork_for_scout(),
            seed,
            sink: ScoutSink {
                bundle: {
                    // W33-01: the scout stitches with the same sub-screen and
                    // per-line registers as the live path.
                    let mut b = FrameBundleBuilder::new(size.0, size.1);
                    b.set_capture_wide(true);
                    b
                },
                keep_events: bands,
            },
            pass: 0,
            rule: StopRule::for_dir(plan.dirs.first().copied().unwrap_or(ScoutDir::Right)),
            status: ScoutStatus::Running,
            frames_total: 0,
            since_merge: 0,
            lag: ScrollLag::default(),
            primed: false,
            bands,
            camera: None,
            y_target: None,
            camera0: None,
            ends: Vec::new(),
            plan,
        }
    }

    #[must_use]
    pub fn status(&self) -> ScoutStatus {
        self.status
    }

    #[must_use]
    pub fn scene(&self) -> SceneId {
        self.scene
    }

    #[must_use]
    pub fn frames_run(&self) -> u64 {
        self.frames_total
    }

    /// `(camera, player)` as the first words of the plan read them now
    /// (diagnostics, tests).
    #[must_use]
    pub fn words(&self) -> (u16, u16) {
        (
            self.plan
                .camera
                .first()
                .map_or(0, |&a| word(&self.stepper, a)),
            self.plan.player_word(&self.stepper, ScoutAxis::X),
        )
    }

    /// W35-06: the plan's camera words on both axes as they read now, `[x, y]`, 0 where the plan
    /// names none. The gate survival check reads it to tell frames the game answered the scout's
    /// pokes (the camera follows) from frames it did not (a scroll of its own, a held player).
    #[must_use]
    pub fn camera_pose(&self) -> [u16; 2] {
        let cam = |axis| {
            self.plan
                .camera_words(axis)
                .first()
                .map_or(0, |&a| word(&self.stepper, a))
        };
        [cam(ScoutAxis::X), cam(ScoutAxis::Y)]
    }

    /// Where the last frame sat on the scout's canvases (diagnostics, tests).
    #[must_use]
    pub fn frames(&self) -> Option<LayerFrames> {
        self.acc.frames()
    }

    /// Read any work-RAM word on the scout's machine (diagnostics, tests).
    #[must_use]
    pub fn peek_word(&self, addr: u32) -> u16 {
        word(&self.stepper, addr)
    }

    /// The scout's own stitched canvases.
    #[must_use]
    pub fn model(&self) -> &CanvasModel {
        &self.acc
    }

    /// Frames stitched since the last [`Self::merge_into`].
    #[must_use]
    pub fn frames_since_merge(&self) -> u32 {
        self.since_merge
    }

    /// Whether this machine is placed by the core's own scroll events (the
    /// NES), the only kind `walk` can drive.
    #[must_use]
    pub fn places_by_bands(&self) -> bool {
        self.bands
    }

    /// The scout's own canvases of its scene (nothing stitched yet: `None`).
    #[must_use]
    pub fn canvases(&self) -> Option<&[Canvas; LAYERS]> {
        self.acc.scene_canvases(self.scene.0)
    }

    /// What has been stitched since the last delta, left in place: the same
    /// cells come again, and more, until [`Self::discard_delta`] or
    /// [`Self::take_delta`]. Counts as a look (the next one is
    /// [`DELTA_EVERY`] frames away).
    #[must_use]
    pub fn peek_delta(&mut self) -> Option<[Canvas; LAYERS]> {
        self.since_merge = 0;
        self.acc.peek_delta(self.scene.0)
    }

    /// Forget what was stitched since the last delta (it was sent, or it was
    /// refused).
    pub fn discard_delta(&mut self) {
        self.since_merge = 0;
        let _ = self.acc.take_dirty();
    }

    /// Start over from the snapshot with another recipe: one pass in `dir`,
    /// stepping `step_px` per frame for at most `frame_budget` frames, on a
    /// clean canvas (a refused attempt leaves nothing in it). The frame
    /// counter keeps counting across restarts.
    ///
    /// # Errors
    /// The snapshot cannot be restored.
    pub fn restart(
        &mut self,
        strategy: ScoutStrategy,
        step_px: u16,
        frame_budget: u32,
        dir: ScoutDir,
    ) -> Result<(), ScoutError> {
        self.plan.strategy = strategy;
        self.plan.step_px = step_px;
        self.plan.frame_budget = frame_budget;
        self.plan.dirs = vec![dir];
        self.stepper
            .restore(&self.snapshot)
            .map_err(|e| ScoutError::Restore(format!("{e:?}")))?;
        if self.bands {
            self.stepper.set_event_mask(EventMask::NONE);
        }
        self.acc = self.seed.fork_for_scout();
        self.pass = 0;
        self.rule = StopRule::for_dir(dir);
        self.status = ScoutStatus::Running;
        self.since_merge = 0;
        self.lag.forget();
        self.primed = false;
        self.camera = None;
        self.y_target = None;
        self.camera0 = None;
        self.ends.clear();
        // Anything the last attempt's frames left in the sink is not this
        // attempt's picture.
        drop(self.sink.bundle.take(self.stepper.frame_count()));
        Ok(())
    }

    /// W35-08: a second scout that starts where this one stands now, with the
    /// scene tracking this one has built on the way (discovery asks whether a
    /// camera scrolls back from the middle of a level, which the level start
    /// cannot say; the model of the start would see the new place as a cut).
    ///
    /// # Errors
    /// The machine cannot be saved, or the ROM cannot be opened.
    pub fn start_from_here(
        &self,
        rom: &[u8],
        plan: ScoutPlan,
        size: (u16, u16),
    ) -> Result<Self, ScoutError> {
        let snapshot = self
            .stepper
            .snapshot()
            .map_err(|e| ScoutError::Restore(format!("{e:?}")))?;
        Self::start(rom, snapshot, plan, &self.acc, self.scene, size)
    }

    /// Ticket W33-07: [`Self::restart`] with a whole other plan (another
    /// player word, another hold), for the discovery trials that try one
    /// recipe after another from the same snapshot.
    ///
    /// # Errors
    /// The snapshot cannot be restored.
    pub fn restart_with(&mut self, plan: ScoutPlan) -> Result<(), ScoutError> {
        let dir = plan.dirs.first().copied().unwrap_or(ScoutDir::Right);
        let (strategy, step, budget) = (plan.strategy, plan.step_px, plan.frame_budget);
        self.plan = plan;
        self.restart(strategy, step, budget, dir)
    }

    /// Fill the unknown cells of `live` for this scout's scene; returns the
    /// number of cells filled. Live cells are never touched.
    pub fn merge_into(&mut self, live: &mut CanvasModel) -> usize {
        self.since_merge = 0;
        let dirty = self.acc.take_dirty();
        let Some(from) = self.acc.scene_canvases(self.scene.0) else {
            return 0;
        };
        live.merge_unknown_within(self.scene.0, from, &dirty)
    }

    /// Ticket W32-08: what was stitched since the last call, as canvases of
    /// their own (see [`CanvasModel::take_delta`]), for the thread that
    /// merges it.
    pub fn take_delta(&mut self) -> Option<[Canvas; LAYERS]> {
        self.since_merge = 0;
        self.acc.take_delta(self.scene.0)
    }

    /// Cells inside the bounding boxes of the scout's canvases (how much it
    /// has seen).
    #[must_use]
    pub fn extent(&self) -> usize {
        self.acc
            .scene_canvases(self.scene.0)
            .map_or(0, |set| set.iter().map(|c| c.width() * c.height()).sum())
    }

    /// W35-04: cells inside the bounding box of the scout's biggest single layer. A layer that is
    /// not the level (a rain overlay, a status bar) adds to [`Self::extent`] without the camera
    /// having followed anyone; the biggest layer is the level's.
    #[must_use]
    pub fn main_extent(&self) -> usize {
        self.layer_dims()
            .iter()
            .map(|&(w, h)| w * h)
            .max()
            .unwrap_or(0)
    }

    /// W35-04: each layer's canvas `(width, height)` in the scout's scene (a probe for the tests).
    #[must_use]
    pub fn layer_dims(&self) -> Vec<(usize, usize)> {
        self.acc
            .scene_canvases(self.scene.0)
            .map_or_else(Vec::new, |set| {
                set.iter().map(|c| (c.width(), c.height())).collect()
            })
    }

    /// Width of the widest canvas of the scout's scene (how far it has seen
    /// sideways; [`Self::extent`] also counts height).
    #[must_use]
    pub fn width(&self) -> usize {
        self.acc
            .scene_canvases(self.scene.0)
            .map_or(0, |set| set.iter().map(|c| c.width()).max().unwrap_or(0))
    }

    /// W35-09: whether the canvas reaches `reach_px` past the live picture on the side `dir`
    /// walks to, measured from where the pass began (the first layer's bounding box: the NES
    /// level layer).
    fn margin_covered(&self, dir: ScoutDir) -> bool {
        let Some(cam0) = self.camera0 else {
            return false;
        };
        let Some(c) = self
            .acc
            .scene_canvases(self.scene.0)
            .and_then(|set| set.first())
        else {
            return false;
        };
        let reach = i64::from(self.plan.reach_px);
        let (left, width) = (c.origin().0, c.width() as i64);
        match dir {
            ScoutDir::Right => left + width >= cam0 + NES_SOURCE_WIDTH + reach,
            ScoutDir::Left => left <= cam0 - reach,
            ScoutDir::Up | ScoutDir::Down => true,
        }
    }

    /// W35-09: the pad bits of a sidestep this frame, if the walker is stuck with a margin to
    /// fill and the zigzag is not spent.
    fn sidestep(&self, dir: ScoutDir) -> Option<u8> {
        if self.plan.reach_px == 0
            || dir.axis() != ScoutAxis::X
            || self.rule.still() + 1 < self.plan.stall_frames
            || self.margin_covered(dir)
        {
            return None;
        }
        // `+ 1`: this frame's own stall count, which the stop rule reads right after it.
        sidestep_pad(self.rule.still() + 1 - self.plan.stall_frames, dir)
    }

    fn end_pass(&mut self, why: ScoutEnd) {
        self.ends.push(why);
        self.pass += 1;
        if self.pass >= self.plan.dirs.len() {
            self.status = ScoutStatus::Finished(why);
            return;
        }
        // Next pass: back to the snapshot, axes and trackers back to the live
        // seed (the canvases built so far are kept).
        if self.stepper.restore(&self.snapshot).is_err() {
            self.status = ScoutStatus::Finished(why);
            return;
        }
        if self.bands {
            self.stepper.set_event_mask(EventMask::NONE);
        }
        self.acc.rewind_to(&self.seed);
        self.camera = None;
        self.y_target = None;
        self.rule = StopRule::for_dir(self.plan.dirs[self.pass]);
        self.lag.forget();
        self.primed = false;
    }

    /// Run ONE scout frame: poke, step, stitch, apply the stop rules.
    /// `scrolls` reads the per-layer scroll off the scout's machine (SNES).
    pub fn step(&mut self, scrolls: ScrollFn<'_>) -> ScoutStatus {
        if let ScoutStatus::Finished(_) = self.status {
            return self.status;
        }
        let dir = self.plan.dirs[self.pass];
        // The scroll registers read AFTER a frame are the ones that draw the
        // NEXT frame (W32-07), the rule `ScrollLag` holds for every stitcher
        // (W32-08). At the start of a pass the restored registers are that
        // read. (The NES needs no lag: the scroll the game writes in vblank,
        // at the END of frame `n`'s slice, draws frame `n + 1`, and the
        // tracker already carries a frame's last write into the next frame's
        // first band; observed on Super Mario Bros. 3: `$00FD` equals the
        // trailing write on 103 of 103 moving frames.)
        if !self.bands && !self.primed {
            self.lag.prime(
                self.stepper.frame_count(),
                FrameScrolls {
                    scene: None,
                    layers: scrolls(&self.stepper),
                },
            );
            self.primed = true;
        }
        let axis = dir.axis();
        let cam = self
            .plan
            .camera_words(axis)
            .first()
            .map_or(0, |&a| word(&self.stepper, a));
        let mut player = self.plan.player_word(&self.stepper, axis);
        if axis == ScoutAxis::Y && self.plan.strategy == ScoutStrategy::PlayerMove {
            // Poke from the last poke (see `y_target`).
            player = *self.y_target.get_or_insert(player);
            self.y_target = Some(self.plan.moved(dir, player));
        }
        for (addr, value) in self.plan.pokes(dir, cam, player) {
            self.stepper.poke_ram(addr, value);
        }
        // Neutral input: the scout's player does nothing of its own, except
        // that the camera of some NES games (Mega Man 2) only follows a
        // player who walks.
        let mut input = InputFrame::empty();
        let mut sidestepping = false;
        if self.bands && self.plan.strategy == ScoutStrategy::Walk {
            let mut pad = dir.nes_pad_bit();
            if let Some(side) = self.sidestep(dir) {
                pad = side;
                sidestepping = true;
            }
            input.ports[0] = u16::from(pad);
        }
        self.rule.defer_stall = sidestepping;
        self.stepper.latch_and_advance_frame(input, &mut self.sink);
        self.frames_total += 1;
        let camera = if self.bands {
            // The stitcher's own placement is the camera. A game that cuts to
            // another scene (a respawn, a transition) ends the pass.
            let bundle = self.sink.bundle.take(self.stepper.frame_count());
            let pad = i64::from(bundle.width.saturating_sub(256) / 2);
            let level = self
                .plan
                .scene_key
                .as_ref()
                .and_then(|k| k.read(|a| self.stepper.peek_bus(a)));
            let hints = rf_enhance::scene_tracker::SceneHints {
                level: level.as_deref(),
                axis_sections: self.plan.scene_key.as_ref().and_then(|k| k.axis_sections),
            };
            let scene = self
                .acc
                .observe_frame(&bundle, &FrameFeed::bands().with_scene_hints(hints));
            if scene != self.scene {
                self.end_pass(ScoutEnd::SceneChanged);
                return self.status;
            }
            if let Some(origin) = self
                .acc
                .frames()
                .filter(|f| f.scene == scene.0)
                .and_then(|f| f.origins[0])
            {
                self.camera = Some(match axis {
                    ScoutAxis::X => origin.0 - pad,
                    ScoutAxis::Y => origin.1,
                });
            }
            if self.camera0.is_none() {
                self.camera0 = self.camera;
            }
            self.camera.unwrap_or(0)
        } else {
            // Without a camera word on this axis the stop rules cannot see a
            // jump (a respawn), only a stall.
            let after = i64::from(
                self.plan
                    .camera_words(axis)
                    .first()
                    .map_or(0, |&a| word(&self.stepper, a)),
            );
            // A jump ends the pass BEFORE this frame is stitched: it may
            // already be a different scene's picture.
            if self.rule.jumped(after) {
                drop(self.sink.bundle.take(self.stepper.frame_count()));
                self.end_pass(ScoutEnd::SceneChanged);
                return self.status;
            }
            let bundle = self.sink.bundle.take(self.stepper.frame_count());
            let read = FrameScrolls {
                scene: None,
                layers: scrolls(&self.stepper),
            };
            let drawn_with = self.lag.drawn_with(self.stepper.frame_count(), read);
            let feed = FrameFeed::registers(scrolls_for(&bundle, &drawn_with, &read));
            self.acc.observe_frame_in(&bundle, self.scene, &feed);
            after
        };
        self.since_merge += 1;
        if self.bands && self.rule.jumped(camera) {
            self.end_pass(ScoutEnd::SceneChanged);
            return self.status;
        }
        let extent = self.extent();
        if let Some(why) = self.rule.observe(&self.plan, camera, extent) {
            self.end_pass(why);
        }
        self.status
    }
}

/// What the scouts have done, shared with whoever asked for them (the app,
/// tests). Relaxed atomics: these are counters, not synchronisation.
#[derive(Debug, Default)]
pub struct ScoutTelemetry {
    /// Scouts started.
    pub started: std::sync::atomic::AtomicU64,
    /// Scouts that ran every pass to its end.
    pub finished: std::sync::atomic::AtomicU64,
    /// Ticket W33-05: scenes entered whose cached canvases already carry a
    /// finished scout, so none was started.
    pub skipped: std::sync::atomic::AtomicU64,
    /// Scouts that ended almost at once (they began mid-transition) and were
    /// retried.
    pub aborted: std::sync::atomic::AtomicU64,
    /// Scout frames run (on the worker), over all scouts.
    pub frames: std::sync::atomic::AtomicU64,
    /// Cells merged into the live canvases.
    pub cells: std::sync::atomic::AtomicU64,
    /// Wall time the workers spent running, microseconds.
    pub wall_us: std::sync::atomic::AtomicU64,
    /// Deltas merged on the core thread.
    pub slices: std::sync::atomic::AtomicU64,
    /// Time the CORE thread spent on scout work (merging deltas), summed,
    /// microseconds: the whole of the scout's cost to the live loop.
    pub core_us: std::sync::atomic::AtomicU64,
    /// The longest single core-thread merge, microseconds.
    pub max_slice_us: std::sync::atomic::AtomicU64,
    /// How the passes of the most recent finished scout ended.
    pub last_ends: std::sync::Mutex<Vec<ScoutEnd>>,
    /// Ticket W33-06: deltas the self-check judged "merge" (>= 99 %).
    pub deltas_merged: std::sync::atomic::AtomicU64,
    /// Deltas judged "slow" (95-99 %): merged, and the step lowered.
    pub deltas_slow: std::sync::atomic::AtomicU64,
    /// Deltas judged "reject" (< 95 %): discarded, never merged.
    pub deltas_rejected: std::sync::atomic::AtomicU64,
    /// Deltas sent with no verdict because the pass had already been accepted
    /// and the delta shared too few cells with the live canvas to be judged.
    pub deltas_unjudged: std::sync::atomic::AtomicU64,
    /// Step probes run (a scout against the same stretch at half the step) and
    /// how many of them rejected the step.
    pub probes: std::sync::atomic::AtomicU64,
    pub probes_rejected: std::sync::atomic::AtomicU64,
    /// Cells that agreed / cells compared, over every judged delta and
    /// probe: the agreement percentage is `agreed / shared`.
    pub agreed_cells: std::sync::atomic::AtomicU64,
    pub shared_cells: std::sync::atomic::AtomicU64,
    /// Scouts that found no rung the check accepted.
    pub gave_up: std::sync::atomic::AtomicU64,
    /// What the most recent scout did: every check with its agreement, and
    /// the rung each pass finished on ([`ScoutReport::summary`]).
    pub last_report: std::sync::Mutex<Option<ScoutReport>>,
}

impl ScoutTelemetry {
    fn add(counter: &std::sync::atomic::AtomicU64, n: u64) {
        counter.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }

    /// Read a counter.
    #[must_use]
    pub fn get(counter: &std::sync::atomic::AtomicU64) -> u64 {
        counter.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Agreement over everything the self-check judged so far, in
    /// thousandths (`None` before anything was judged).
    #[must_use]
    pub fn agreement_permille(&self) -> Option<u64> {
        let shared = Self::get(&self.shared_cells);
        (shared > 0).then(|| Self::get(&self.agreed_cells) * 1000 / shared)
    }

    /// The last scout's report line (checks, agreement, rung), if one ran.
    #[must_use]
    pub fn last_summary(&self) -> Option<String> {
        self.last_report
            .lock()
            .ok()
            .and_then(|r| r.as_ref().map(ScoutReport::summary))
    }
}

/// A plan plus where to report to.
#[derive(Debug, Clone)]
pub struct ScoutSetup {
    pub plan: ScoutPlan,
    pub telemetry: std::sync::Arc<ScoutTelemetry>,
}

/// Camera words are compared with the live layer scroll to this tolerance
/// before a scout starts (the game may still be loading).
const CAMERA_TOLERANCE_PX: i64 = 8;
/// How many times a scene's start is retried before it is given up.
const MAX_ATTEMPTS: u32 = 5;
/// Live frames between retries.
const RETRY_FRAMES: u32 = 60;
/// The start conditions must have held for this many live frames running
/// before a scout starts (a restart or fade-in is not "ready" yet).
const READY_FRAMES: u32 = 30;
/// A scout that ends having run fewer frames than this started in the middle
/// of a transition: it is retried, not counted as the scene's scout.
const MIN_USEFUL_FRAMES: u64 = 60;
/// Scout frames between deltas sent to the core thread.
const DELTA_EVERY: u32 = 60;
/// Messages the worker may have in flight; it waits when the core thread has
/// not merged them yet (a scout has no real-time need).
const CHANNEL_DEPTH: usize = 4;
/// Most messages the core thread takes from the worker per live frame.
const MSGS_PER_TICK: usize = 2;
/// The stretch (pixels of player travel past the lead-in) a step probe
/// scouts at the step and at half of it.
const PROBE_PX: u32 = 640;
/// Lead-in of a probe: the player walks to where the camera starts to follow.
const PROBE_LEAD_PX: u32 = 256;
/// Most rung/step changes one pass may go through (3 rungs x 4 steps, with
/// room): a backstop for law 8, the ladder itself is finite.
const MAX_ATTEMPTS_PER_PASS: u32 = 24;

/// Reads the per-layer scroll the stitcher needs off a scout's machine (the
/// same function the live path uses, so the two agree on layers). A plain
/// `fn` so it can go to the worker thread.
pub type ScrollReader = fn(&EmuStepper) -> Option<[Option<LayerScroll>; LAYERS]>;

/// What the worker sends the core thread.
enum ScoutMsg {
    /// Cells stitched since the last delta (see `CanvasModel::take_delta`),
    /// already judged by the worker's self-check.
    Delta {
        scene: u64,
        canvases: Box<[Canvas; LAYERS]>,
    },
    /// Every pass is over.
    Done {
        ends: Vec<ScoutEnd>,
        frames: u64,
        report: Box<ScoutReport>,
    },
    /// The machine could not be opened or restored, or the scout began in the
    /// middle of a transition.
    Failed,
}

/// All the worker needs, owned (the scout machine is built ON the worker
/// from the ROM bytes and the snapshot).
struct ScoutJob {
    rom: Vec<u8>,
    snapshot: MachineSnapshot,
    plan: ScoutPlan,
    /// The live model's axes and trackers at the snapshot, no canvases.
    seed: CanvasModel,
    /// The live canvases at the snapshot, for the self-check.
    reference: Option<ScoutReference>,
    /// The rung that finished this scene's last scout (W33-05/06 memo).
    hint: Option<(ScoutStrategy, u16)>,
    scene: SceneId,
    size: (u16, u16),
    reader: ScrollReader,
    telemetry: std::sync::Arc<ScoutTelemetry>,
}

/// A running worker. Dropping it cancels the worker (it notices within a
/// frame, and a worker blocked on a full channel within a few ms) and lets
/// it go: nothing it still holds is shared with anything.
struct ScoutRun {
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    rx: std::sync::mpsc::Receiver<ScoutMsg>,
}

impl Drop for ScoutRun {
    fn drop(&mut self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Send, waiting while the channel is full; `false` = cancelled or the core
/// thread is gone.
fn send_or_cancel(
    tx: &std::sync::mpsc::SyncSender<ScoutMsg>,
    cancel: &std::sync::atomic::AtomicBool,
    mut msg: ScoutMsg,
) -> bool {
    use std::sync::mpsc::TrySendError;
    // Bounded: every iteration either returns or sleeps until cancelled.
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        match tx.try_send(msg) {
            Ok(()) => return true,
            Err(TrySendError::Full(m)) => {
                msg = m;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

/// The worker stopped: cancelled, the core thread went away, or the machine
/// could not be restored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Cancelled,
    Broken,
}

/// How one pass of the worker ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassEnd {
    /// The pass ran to its end on this rung at this step.
    Accepted(ScoutEnd, (ScoutStrategy, u16)),
    /// No rung was accepted.
    Failed,
    /// The scout began in the middle of a transition.
    Aborted,
}

/// What one attempt at a pass came to.
enum PassRun {
    Done(ScoutEnd, (ScoutStrategy, u16)),
    /// The check refused it: the ladder has moved.
    Refused,
    Aborted,
}

/// The worker: one scout machine reused for every probe and attempt (each a
/// [`Scout::restart`]), the live reference its checks compare with, and the
/// report it ships when it is done.
struct Worker<'a> {
    scout: Scout,
    base: ScoutPlan,
    reference: Option<ScoutReference>,
    reader: ScrollReader,
    tx: &'a std::sync::mpsc::SyncSender<ScoutMsg>,
    cancel: &'a std::sync::atomic::AtomicBool,
    telemetry: &'a ScoutTelemetry,
    report: ScoutReport,
    /// Frames already counted into the telemetry.
    counted: u64,
    /// The `(rung, step)` a probe already vouched for.
    validated: Option<(ScoutStrategy, u16)>,
    scene: u64,
}

/// What the worker does with a delta (the rules of the self-check, apart from
/// the machine, so they are tested alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// The pass's first verdict is "reject": the rung is refused at this
    /// step and the ladder moves.
    Refuse,
    /// A delta of an accepted pass judged "reject": dropped, never merged.
    Drop,
    /// Merge it. `lower`: it was the pass's first verdict and "slow".
    Send { lower: bool },
    /// No verdict from the live canvas, and none to wait for: merge it (the
    /// step was vouched for by a probe, or there was nothing slower to check
    /// it against).
    SendUnjudged,
    /// Nothing was stitched: nothing to send, no fault.
    Nothing,
    /// No verdict yet, the live canvas may still give one: keep the delta for
    /// the next look.
    Hold,
}

/// The §3.3 rules for one delta: `verdict` from [`check_delta`], whether the
/// pass has been accepted already, whether it just ended, whether the delta
/// holds no cell, whether the live canvas holds enough cells to ever judge
/// one. A scene the live game has not drawn (the player stands still at the
/// start of a level: an unchanged scroll stitches nothing) can judge nothing,
/// and a scout is not refused for that; the probe is its check.
fn disposition(
    verdict: Option<ScoutVerdict>,
    accepted: bool,
    ended: bool,
    empty: bool,
    judgeable: bool,
) -> Disposition {
    match verdict {
        Some(ScoutVerdict::Reject) if !accepted => Disposition::Refuse,
        Some(ScoutVerdict::Reject) => Disposition::Drop,
        Some(v) => Disposition::Send {
            lower: v == ScoutVerdict::SlowDown && !accepted,
        },
        None if empty => Disposition::Nothing,
        None if accepted || ended || !judgeable => Disposition::SendUnjudged,
        None => Disposition::Hold,
    }
}

impl Worker<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Report the frames run since the last call.
    fn count_frames(&mut self) {
        let run = self.scout.frames_run();
        ScoutTelemetry::add(&self.telemetry.frames, run - self.counted);
        self.counted = run;
    }

    /// Keep a check in the report and the counters.
    fn note(
        &mut self,
        what: &'static str,
        strategy: ScoutStrategy,
        step_px: u16,
        check: DeltaCheck,
    ) {
        let t = self.telemetry;
        ScoutTelemetry::add(&t.agreed_cells, check.agreed as u64);
        ScoutTelemetry::add(&t.shared_cells, check.shared as u64);
        if what == "probe" {
            ScoutTelemetry::add(&t.probes, 1);
            if check.verdict == Some(ScoutVerdict::Reject) {
                ScoutTelemetry::add(&t.probes_rejected, 1);
            }
        } else {
            let counter = match check.verdict {
                Some(ScoutVerdict::Merge) => Some(&t.deltas_merged),
                Some(ScoutVerdict::SlowDown) => Some(&t.deltas_slow),
                Some(ScoutVerdict::Reject) => Some(&t.deltas_rejected),
                None => None,
            };
            if let Some(c) = counter {
                ScoutTelemetry::add(c, 1);
            }
        }
        self.report.attempts.push(Attempt {
            what,
            strategy,
            step_px,
            check,
        });
    }

    fn send(&self, msg: ScoutMsg) -> Result<(), Stop> {
        if send_or_cancel(self.tx, self.cancel, msg) {
            Ok(())
        } else {
            Err(Stop::Cancelled)
        }
    }

    /// Restart the machine on a recipe.
    fn restart(
        &mut self,
        strategy: ScoutStrategy,
        step_px: u16,
        budget: u32,
        dir: ScoutDir,
    ) -> Result<(), Stop> {
        self.scout
            .restart(strategy, step_px, budget, dir)
            .map_err(|_| Stop::Broken)
    }

    /// The canvases after the first `frames` frames of a pass at `step_px`.
    fn canvases_after(
        &mut self,
        strategy: ScoutStrategy,
        step_px: u16,
        dir: ScoutDir,
    ) -> Result<Option<[Canvas; LAYERS]>, Stop> {
        let frames = ((PROBE_PX + PROBE_LEAD_PX) / u32::from(step_px.max(1))).max(1);
        self.restart(strategy, step_px, frames, dir)?;
        // Bounded: the pass's frame budget is `frames`.
        for _ in 0..=frames {
            if self.cancelled() {
                return Err(Stop::Cancelled);
            }
            if self.scout.step(&self.reader) != ScoutStatus::Running {
                break;
            }
            std::thread::yield_now();
        }
        self.count_frames();
        Ok(self.scout.canvases().cloned())
    }

    /// Check `step_px` against the same stretch at `reference_step`.
    fn probe(
        &mut self,
        strategy: ScoutStrategy,
        step_px: u16,
        reference_step: u16,
        dir: ScoutDir,
    ) -> Result<DeltaCheck, Stop> {
        let fast = self.canvases_after(strategy, step_px, dir)?;
        let slow = self.canvases_after(strategy, reference_step, dir)?;
        let check = match (fast, slow) {
            (Some(fast), Some(slow)) => check_delta(&slow, &fast, [true; LAYERS]),
            _ => DeltaCheck::default(),
        };
        self.note("probe", strategy, step_px, check);
        Ok(check)
    }

    /// Judge the delta the scout holds and send, hold or discard it.
    /// `true` = the attempt is refused (the ladder has moved); `false` =
    /// carry on.
    fn deliver(
        &mut self,
        ladder: &mut Ladder,
        (strategy, step): (ScoutStrategy, u16),
        accepted: &mut bool,
        ended: bool,
    ) -> Result<bool, Stop> {
        let Some(delta) = self.scout.peek_delta() else {
            return Ok(false);
        };
        let check = self
            .reference
            .as_ref()
            .map_or_else(DeltaCheck::default, |r| {
                check_delta(&r.layers, &delta, r.trusted)
            });
        let empty = delta.iter().all(|c| c.width() == 0);
        let judgeable = self.report.reference_cells >= MIN_SHARED_CELLS;
        match disposition(check.verdict, *accepted, ended, empty, judgeable) {
            Disposition::Refuse => {
                // The pass's first verdict: the rung is refused at this step.
                self.note("delta", strategy, step, check);
                self.scout.discard_delta();
                ladder.reject();
                return Ok(true);
            }
            Disposition::Drop => {
                // The pass was accepted on its first judged delta. A later
                // one that overlaps the live canvas again (its edge, where
                // the newest live columns may still be half drawn) is
                // dropped, never merged; the pass is not thrown away for it.
                self.note("delta", strategy, step, check);
                self.scout.discard_delta();
                return Ok(false);
            }
            Disposition::Send { lower } => {
                self.note("delta", strategy, step, check);
                if lower {
                    ladder.lower();
                }
                *accepted = true;
            }
            Disposition::SendUnjudged => {
                ScoutTelemetry::add(&self.telemetry.deltas_unjudged, 1);
            }
            Disposition::Nothing => return Ok(false),
            Disposition::Hold => return Ok(false), // it may be judged next time
        }
        self.scout.discard_delta();
        if empty {
            return Ok(false);
        }
        self.send(ScoutMsg::Delta {
            scene: self.scene,
            canvases: Box::new(delta),
        })?;
        Ok(false)
    }

    /// One attempt at a pass on the ladder's current rung and step.
    fn run_pass(
        &mut self,
        ladder: &mut Ladder,
        recipe: (ScoutStrategy, u16),
        dir: ScoutDir,
    ) -> Result<PassRun, Stop> {
        let budget = ladder.scaled_budget(self.base.frame_budget);
        self.restart(recipe.0, recipe.1, budget, dir)?;
        let first_frame = self.scout.frames_run();
        let mut accepted = false;
        // A pass ends by its frame budget at the latest, so this cap is only
        // a backstop for law 8.
        for _ in 0..=u64::from(budget) {
            if self.cancelled() {
                return Err(Stop::Cancelled);
            }
            let status = self.scout.step(&self.reader);
            let ScoutStatus::Finished(end) = status else {
                if self.scout.frames_since_merge() >= DELTA_EVERY {
                    self.count_frames();
                    if self.deliver(ladder, recipe, &mut accepted, false)? {
                        return Ok(PassRun::Refused);
                    }
                }
                // Lower priority: a scout has no real-time need, so give
                // the core thread's core back after every frame.
                std::thread::yield_now();
                continue;
            };
            self.count_frames();
            if end == ScoutEnd::SceneChanged
                && self.scout.frames_run() - first_frame < MIN_USEFUL_FRAMES
            {
                return Ok(PassRun::Aborted);
            }
            return if self.deliver(ladder, recipe, &mut accepted, true)? {
                Ok(PassRun::Refused)
            } else {
                Ok(PassRun::Done(end, recipe))
            };
        }
        // Unreachable in practice (the budget ends the pass); a pass that
        // somehow did not is a failed rung.
        ladder.reject();
        Ok(PassRun::Refused)
    }

    /// One pass in `dir`, climbing the ladder until a rung is accepted.
    fn pass(&mut self, ladder: &mut Ladder, dir: ScoutDir) -> Result<PassEnd, Stop> {
        // Bounded: every iteration either returns or moves the ladder, which
        // only ever moves forward.
        for _ in 0..MAX_ATTEMPTS_PER_PASS {
            let Some(recipe) = ladder.current() else {
                return Ok(PassEnd::Failed);
            };
            if let Some(reference_step) = ladder.reference_step() {
                if self.validated != Some(recipe) {
                    let check = self.probe(recipe.0, recipe.1, reference_step, dir)?;
                    if check.verdict == Some(ScoutVerdict::Reject) {
                        ladder.reject();
                        continue;
                    }
                    self.validated = Some(recipe);
                }
            }
            match self.run_pass(ladder, recipe, dir)? {
                PassRun::Done(end, recipe) => return Ok(PassEnd::Accepted(end, recipe)),
                PassRun::Refused => {}
                PassRun::Aborted => return Ok(PassEnd::Aborted),
            }
        }
        Ok(PassEnd::Failed)
    }

    /// Every pass; `None` = the scout began in the middle of a transition.
    fn run(&mut self, mut ladder: Ladder) -> Result<Option<Vec<ScoutEnd>>, Stop> {
        let mut ends = Vec::new();
        let dirs = self.base.dirs.clone();
        // A pass that fails leaves the ladder spent; the next pass starts
        // from the rung the last accepted one finished on.
        let mut good = ladder.clone();
        for (i, &dir) in dirs.iter().enumerate() {
            ladder = good.clone();
            match self.pass(&mut ladder, dir)? {
                PassEnd::Accepted(end, recipe) => {
                    self.report.passes.push(PassReport {
                        dir,
                        finished_on: Some(recipe),
                        end: Some(end),
                    });
                    ends.push(end);
                    good = ladder.clone();
                }
                PassEnd::Failed => {
                    self.report.passes.push(PassReport {
                        dir,
                        finished_on: None,
                        end: None,
                    });
                    if i == 0 {
                        self.report.gave_up = true;
                        break;
                    }
                }
                PassEnd::Aborted => return Ok(None),
            }
        }
        Ok(Some(ends))
    }
}

/// Known cells of the layers a check may compare with.
fn reference_cells(r: Option<&ScoutReference>) -> usize {
    r.map_or(0, |r| {
        r.layers
            .iter()
            .zip(r.trusted)
            .filter(|(_, ok)| *ok)
            .map(|(c, _)| c.cells().iter().flatten().count())
            .sum()
    })
}

/// The worker body: open, restore, run the passes through the ladder, ship
/// the deltas the self-check accepted.
fn run_worker(
    job: ScoutJob,
    tx: &std::sync::mpsc::SyncSender<ScoutMsg>,
    cancel: &std::sync::atomic::AtomicBool,
) {
    let t0 = std::time::Instant::now();
    let ScoutJob {
        rom,
        snapshot,
        plan,
        seed,
        reference,
        hint,
        scene,
        size,
        reader,
        telemetry,
    } = job;
    let started = Scout::start(&rom, snapshot, plan.clone(), &seed, scene, size);
    let Ok(scout) = started else {
        let _ = send_or_cancel(tx, cancel, ScoutMsg::Failed);
        return;
    };
    let axis = plan.dirs.first().map_or(ScoutAxis::X, |d| d.axis());
    let rungs = rungs_for(&plan, axis, scout.places_by_bands(), hint.map(|h| h.0));
    // The profile's step is the X recipe's (Mega Man 2's is 1 px, a walk). A
    // vertical pass starts at the default and lets the ladder halve it.
    let start_step = match hint {
        Some((s, step)) if rungs.first() == Some(&s) => step,
        _ if axis == ScoutAxis::Y => crate::scout_ladder::DEFAULT_STEP_PX,
        _ => plan.step_px,
    };
    let known = reference_cells(reference.as_ref());
    let mut worker = Worker {
        scout,
        base: plan,
        reference,
        reader,
        tx,
        cancel,
        telemetry: &telemetry,
        report: ScoutReport {
            reference_cells: known,
            ..ScoutReport::default()
        },
        counted: 0,
        validated: None,
        scene: scene.0,
    };
    let outcome = worker.run(Ladder::new(rungs, start_step));
    ScoutTelemetry::add(&telemetry.wall_us, t0.elapsed().as_micros() as u64);
    let msg = match outcome {
        Ok(Some(ends)) => ScoutMsg::Done {
            ends,
            frames: worker.scout.frames_run(),
            report: Box::new(std::mem::take(&mut worker.report)),
        },
        Ok(None) | Err(Stop::Broken) => ScoutMsg::Failed,
        Err(Stop::Cancelled) => return,
    };
    let _ = send_or_cancel(tx, cancel, msg);
}

/// W35-11: whether a frame's picture is one flat colour ([`LiveView::blank`]). Stops at the first
/// pixel that differs, so a picture with anything on it costs a couple of reads.
#[must_use]
pub fn picture_is_flat(video: &[rf_core_api::PpuPixel]) -> bool {
    video
        .first()
        .is_none_or(|a| video.iter().all(|p| p.palette_index == a.palette_index))
}

/// Whether the `start_when` rows hold: for every address, the byte reads one of the values the
/// rows name for it (W35-11: a gate on one address may be open on several values).
pub fn start_gate_open(rows: &[(u32, u8)], peek: impl Fn(u32) -> u8) -> bool {
    rows.iter().all(|&(addr, _)| {
        let now = peek(addr);
        rows.iter().any(|&(a, v)| a == addr && v == now)
    })
}

/// Decides WHEN to scout, hands the work to a worker thread and merges what
/// it sends back. Driven once per live frame from the core thread.
#[derive(Default)]
pub struct ScoutDriver {
    setup: Option<ScoutSetup>,
    scene: Option<u64>,
    settle_left: u32,
    attempts: u32,
    /// This scene's scout has finished (or been given up): never again.
    done: bool,
    run: Option<ScoutRun>,
    /// Live frames the start conditions have held, running.
    ready: u32,
}

/// The live machine and its surroundings, for one [`ScoutDriver::tick`].
pub struct LiveView<'a> {
    pub stepper: &'a EmuStepper,
    pub rom: &'a [u8],
    pub canvases: &'a mut CanvasModel,
    pub scene: SceneId,
    /// The live frame's `(width, height)`.
    pub size: (u16, u16),
    pub scrolls: ScrollReader,
    /// W35-11: the live picture is one flat colour (a black screen between scenes, a fade at its
    /// darkest): there is nothing on it to scout, whatever the game's memory says.
    pub blank: bool,
}

impl ScoutDriver {
    /// Arm (or, with `None`, disarm) scouting. Forgets the scene, so the
    /// current one is scouted afresh; a running worker is cancelled.
    pub fn set(&mut self, setup: Option<ScoutSetup>) {
        self.setup = setup;
        self.scene = None;
        self.run = None;
        self.done = false;
        self.ready = 0;
    }

    /// Whether a scout is running now.
    #[must_use]
    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    /// One live frame has just run.
    pub fn tick(&mut self, live: LiveView<'_>) {
        let Some(setup) = self.setup.clone() else {
            return;
        };
        if live.scene == rf_enhance::scene_tracker::SETTLING {
            // A cut that has not settled belongs to no scene yet (ticket
            // W33-09): nothing starts, and a pass that is running is left to
            // the scene it was started for.
            return;
        }
        if self.scene != Some(live.scene.0) {
            // A new scene: whatever ran belongs to the old one.
            self.scene = Some(live.scene.0);
            self.run = None;
            self.done = false;
            self.attempts = 0;
            self.ready = 0;
            self.settle_left = setup.plan.settle_frames;
            // Ticket W33-05: the cache restored this scene with a scout's
            // work in it. A second visit starts nothing.
            if live.canvases.scene_scouted(live.scene.0) {
                self.done = true;
                ScoutTelemetry::add(&setup.telemetry.skipped, 1);
            }
        }
        if self.done {
            return;
        }
        if self.run.is_none() {
            if self.settle_left > 0 {
                self.settle_left -= 1;
                return;
            }
            if !self.start(&setup, &live) {
                return;
            }
        }
        self.pump(&setup, live.canvases, live.scene);
    }

    /// Merge what a running worker has sent without a live frame (the game
    /// is paused: the worker goes on, its deltas should not pile up). Never
    /// starts a scout.
    pub fn poll(&mut self, canvases: &mut CanvasModel) {
        let (Some(setup), Some(scene)) = (self.setup.clone(), self.scene) else {
            return;
        };
        self.pump(&setup, canvases, SceneId(scene));
    }

    /// Try to start the scene's scout; `false` = not now.
    fn start(&mut self, setup: &ScoutSetup, live: &LiveView<'_>) -> bool {
        // Not ready (the player is dead, in a cutscene, the picture is black): wait, without
        // using up a retry; the live game will get there or it will not.
        if live.blank || !start_gate_open(&setup.plan.start_when, |a| live.stepper.peek_bus(a)) {
            self.ready = 0;
            return false;
        }
        self.ready += 1;
        if self.ready < READY_FRAMES {
            return false;
        }
        let axis = scene_axis(&setup.plan, live);
        // A SNES scout needs the game's camera word to agree with a live
        // layer (the NES's camera is the stitcher's own placement).
        if live.stepper.bg_layer_views().is_some()
            && !camera_matches(&setup.plan, axis, live.stepper, live.scrolls)
        {
            self.retry();
            return false;
        }
        let Ok(snapshot) = live.stepper.snapshot() else {
            self.retry();
            return false;
        };
        let mut plan = setup.plan.clone();
        plan.dirs = plan.dirs_for(axis);
        let job = ScoutJob {
            rom: live.rom.to_vec(),
            snapshot,
            plan,
            seed: live.canvases.fork_for_scout(),
            reference: live.canvases.scout_reference(live.scene.0),
            hint: live
                .canvases
                .scene_scout_memo(live.scene.0)
                .and_then(|m| Some((rung_of_code(m.rung)?, u16::from(m.step_px)))),
            scene: live.scene,
            size: live.size,
            reader: live.scrolls,
            telemetry: setup.telemetry.clone(),
        };
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::sync_channel(CHANNEL_DEPTH);
        let worker_cancel = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("rf-scout".into())
            .spawn(move || run_worker(job, &tx, &worker_cancel));
        match spawned {
            Ok(_) => {
                ScoutTelemetry::add(&setup.telemetry.started, 1);
                self.run = Some(ScoutRun { cancel, rx });
                true
            }
            Err(_) => {
                self.retry();
                false
            }
        }
    }

    fn retry(&mut self) {
        self.attempts += 1;
        self.settle_left = RETRY_FRAMES;
        self.ready = 0;
        if self.attempts >= MAX_ATTEMPTS {
            self.done = true;
        }
    }

    /// Merge what the worker has sent (a couple of messages per live frame:
    /// the core thread's only scout work).
    fn pump(&mut self, setup: &ScoutSetup, live: &mut CanvasModel, scene: SceneId) {
        use std::sync::mpsc::TryRecvError;
        let t = &setup.telemetry;
        // Bounded: MSGS_PER_TICK iterations.
        for _ in 0..MSGS_PER_TICK {
            let Some(run) = self.run.as_ref() else {
                return;
            };
            match run.rx.try_recv() {
                Ok(ScoutMsg::Delta { scene: s, canvases }) => {
                    if s != scene.0 {
                        continue;
                    }
                    let t0 = std::time::Instant::now();
                    let merged = live.merge_unknown_from(s, &canvases);
                    let us = t0.elapsed().as_micros() as u64;
                    ScoutTelemetry::add(&t.cells, merged as u64);
                    ScoutTelemetry::add(&t.slices, 1);
                    ScoutTelemetry::add(&t.core_us, us);
                    t.max_slice_us
                        .fetch_max(us, std::sync::atomic::Ordering::Relaxed);
                }
                Ok(ScoutMsg::Done {
                    ends,
                    frames,
                    report,
                }) => {
                    self.run = None;
                    let memo = if report.gave_up {
                        // No rung passed the check: margins stay on live
                        // cells, and the scene is not scouted again.
                        ScoutTelemetry::add(&t.gave_up, 1);
                        Some(rf_enhance::persistence::ScoutMemo {
                            rung: rf_enhance::persistence::ScoutMemo::GAVE_UP,
                            step_px: 0,
                        })
                    } else if frames >= MIN_USEFUL_FRAMES {
                        ScoutTelemetry::add(&t.finished, 1);
                        if let Ok(mut last) = t.last_ends.lock() {
                            *last = ends;
                        }
                        report
                            .winner()
                            .map(|(s, step)| rf_enhance::persistence::ScoutMemo {
                                rung: crate::scout_ladder::rung_code(s),
                                step_px: u8::try_from(step).unwrap_or(u8::MAX),
                            })
                    } else {
                        None
                    };
                    let finished = report.gave_up || frames >= MIN_USEFUL_FRAMES;
                    if let Ok(mut last) = t.last_report.lock() {
                        *last = Some(*report);
                    }
                    if finished {
                        self.done = true;
                        // Remembered with the scene (and written now): the
                        // next visit skips the scout, and a later one starts
                        // on the rung that worked.
                        live.mark_scouted_with(scene.0, memo);
                    } else {
                        // It began in the middle of a transition: try again later.
                        ScoutTelemetry::add(&t.aborted, 1);
                        self.retry();
                    }
                    return;
                }
                Ok(ScoutMsg::Failed) | Err(TryRecvError::Disconnected) => {
                    // Could not start, or the worker died without finishing.
                    ScoutTelemetry::add(&t.aborted, 1);
                    self.run = None;
                    self.retry();
                    return;
                }
                Err(TryRecvError::Empty) => return,
            }
        }
    }
}

/// Ticket W33-08: the axis to scout the live scene along. The scene tracker
/// names the axis the screen has been scrolling along (the rule behind
/// `[scene] sections = "axis"`); a scene it cannot name yet, or one the recipe
/// has no Y words for, is scouted along X as before.
fn scene_axis(plan: &ScoutPlan, live: &LiveView<'_>) -> ScoutAxis {
    let bands = live.stepper.bg_layer_views().is_none();
    if live.canvases.scene_scrolls_vertically() == Some(true) && plan.can_scout(ScoutAxis::Y, bands)
    {
        ScoutAxis::Y
    } else {
        ScoutAxis::X
    }
}

/// Whether the game's camera word on `axis` agrees with a stitched layer's live scroll (the game
/// is in a level and the recipe's address is right). W35-04: ANY layer: A Link to the Past's first
/// stitched layer is a rain overlay that scrolls at its own pace, and its camera is the second.
fn camera_matches(
    plan: &ScoutPlan,
    axis: ScoutAxis,
    stepper: &EmuStepper,
    scrolls: ScrollReader,
) -> bool {
    let Some(&addr) = plan.camera_words(axis).first() else {
        return true;
    };
    let Some(layers) = scrolls(stepper) else {
        return false;
    };
    layers.into_iter().flatten().any(|layer| {
        let (wrap, live) = match axis {
            ScoutAxis::X => (layer.tilemap_width, layer.hofs),
            ScoutAxis::Y => (layer.tilemap_height, layer.vofs),
        };
        let wrap = i64::from(wrap.max(1));
        let cam = i64::from(word(stepper, addr)).rem_euclid(wrap);
        let d = (cam - i64::from(live)).rem_euclid(wrap);
        d.min(wrap - d) <= CAMERA_TOLERANCE_PX
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(strategy: ScoutStrategy) -> ScoutPlan {
        ScoutPlan {
            strategy,
            camera: vec![0x7E_001A, 0x7E_1462],
            player_x: vec![0x7E_0094],
            player_x_hi: Vec::new(),
            camera_y: Vec::new(),
            player_y: Vec::new(),
            player_y_hi: Vec::new(),
            step_px: 4,
            frame_budget: 10,
            stall_frames: 3,
            settle_frames: 0,
            dirs: vec![ScoutDir::Right],
            hold: vec![(0x7E_0100, 7)],
            start_when: Vec::new(),
            pin: false,
            scene_key: None,
            reach_px: 0,
        }
    }

    #[test]
    fn camera_write_pokes_every_camera_word_and_the_hold_table() {
        let p = plan(ScoutStrategy::CameraWrite).pokes(ScoutDir::Right, 0x01FE, 0x0300);
        // 0x01FE + 4 = 0x0202, little endian, to both camera words.
        assert_eq!(
            p,
            vec![
                (0x7E_001A, 0x02),
                (0x7E_001B, 0x02),
                (0x7E_1462, 0x02),
                (0x7E_1463, 0x02),
                (0x7E_0100, 7)
            ]
        );
    }

    /// A plan with Y rows: SMW's 16-bit player Y (the hold rows are its two
    /// bytes) and Mega Man 2's one-byte one.
    fn y_plan(strategy: ScoutStrategy, wide: bool) -> ScoutPlan {
        let mut p = plan(strategy);
        p.camera_y = vec![0x7E_001C];
        p.player_y = vec![0x7E_0096];
        if wide {
            p.player_y_hi = vec![0x7E_0097];
        }
        p.hold = vec![(0x7E_0096, 150), (0x7E_0097, 0), (0x7E_0100, 7)];
        p
    }

    #[test]
    fn a_y_pass_releases_the_holds_on_the_players_y_word_and_keeps_the_rest() {
        let p = y_plan(ScoutStrategy::PlayerMove, true);
        // Going right the hold pins him high, as before.
        let right = p.pokes(ScoutDir::Right, 0, 0x0100);
        assert!(right.contains(&(0x7E_0096, 150)) && right.contains(&(0x7E_0097, 0)));
        // Going down the word is moved by the recipe: 0x0150 + 4, both bytes,
        // and no hold on either byte, only the unrelated one.
        let down = p.pokes(ScoutDir::Down, 0, 0x0150);
        assert_eq!(
            down,
            vec![(0x7E_0096, 0x54), (0x7E_0097, 0x01), (0x7E_0100, 7)]
        );
        // Going up it is 4 less.
        let up = p.pokes(ScoutDir::Up, 0, 0x0150);
        assert_eq!(up[..2], [(0x7E_0096, 0x4C), (0x7E_0097, 0x01)]);
    }

    #[test]
    fn a_player_y_with_no_high_row_is_one_byte_held_inside_it() {
        let p = y_plan(ScoutStrategy::PlayerMove, false);
        assert_eq!(p.pokes(ScoutDir::Down, 0, 0x54)[0], (0x7E_0096, 0x58));
        // No second poke for a high byte, and the value stops at 0xFF.
        let p2 = p.pokes(ScoutDir::Down, 0, 0xFE);
        assert_eq!(p2[0], (0x7E_0096, 0xFF));
        // The recipe writes no high byte; the hold row on `$7E0097` is not a
        // `player_y` byte here, so it stays (a profile that means it as the
        // high half names it `player_y_hi`).
        assert_eq!(p2, vec![(0x7E_0096, 0xFF), (0x7E_0097, 0), (0x7E_0100, 7)]);
        assert_eq!(p.moved(ScoutDir::Up, 2), 0);
        // The X recipe of the same plan still has the whole 16 bits.
        assert_eq!(p.moved(ScoutDir::Right, 0xFFFE), 0xFFFF);
    }

    #[test]
    fn camera_write_on_the_y_axis_writes_the_y_camera_words() {
        let p = y_plan(ScoutStrategy::CameraWrite, true).pokes(ScoutDir::Down, 0x00F0, 0);
        assert_eq!(p[..2], [(0x7E_001C, 0xF4), (0x7E_001D, 0x00)]);
    }

    #[test]
    fn a_scene_gets_the_directions_of_its_own_axis() {
        let mut p = plan(ScoutStrategy::PlayerMove);
        p.dirs = vec![ScoutDir::Right];
        assert_eq!(p.dirs_for(ScoutAxis::X), vec![ScoutDir::Right]);
        // The profile names none on Y: forward (down) first, then back.
        assert_eq!(p.dirs_for(ScoutAxis::Y), vec![ScoutDir::Down, ScoutDir::Up]);
        p.dirs = vec![ScoutDir::Up, ScoutDir::Right];
        assert_eq!(p.dirs_for(ScoutAxis::Y), vec![ScoutDir::Up]);
        assert_eq!(ScoutDir::parse("up"), Some(ScoutDir::Up));
        assert_eq!(ScoutDir::parse("sideways"), None);
    }

    #[test]
    fn a_recipe_without_y_words_cannot_scout_a_vertical_scene_unless_it_walks() {
        let p = plan(ScoutStrategy::PlayerMove);
        assert!(p.can_scout(ScoutAxis::X, false));
        assert!(!p.can_scout(ScoutAxis::Y, false));
        assert!(p.can_scout(ScoutAxis::Y, true), "a NES pad can walk down");
        assert!(y_plan(ScoutStrategy::PlayerMove, true).can_scout(ScoutAxis::Y, false));
    }

    #[test]
    fn the_pad_bits_of_all_four_directions_are_the_nes_ones() {
        assert_eq!(ScoutDir::Up.nes_pad_bit(), 1 << 4);
        assert_eq!(ScoutDir::Down.nes_pad_bit(), 1 << 5);
        assert_eq!(ScoutDir::Left.nes_pad_bit(), 1 << 6);
        assert_eq!(ScoutDir::Right.nes_pad_bit(), 1 << 7);
        assert_eq!(ScoutDir::Down.axis(), ScoutAxis::Y);
    }

    #[test]
    fn a_vertical_pass_gets_a_lead_in_before_the_stall_rule_counts() {
        let mut p = plan(ScoutStrategy::PlayerMove);
        p.frame_budget = 10_000;
        // X: stalls after `stall_frames` still frames, as before.
        let mut r = StopRule::for_dir(ScoutDir::Right);
        let ends: Vec<_> = (0..5).map(|_| r.observe(&p, 0, 100)).collect();
        assert_eq!(
            ends,
            vec![
                None,
                None,
                None,
                Some(ScoutEnd::Stalled),
                Some(ScoutEnd::Stalled)
            ]
        );
        // Y: nothing grows for 299 frames (the player is walking to the edge
        // of the screen) and the pass is still alive; the 300th ends it.
        let mut r = StopRule::for_dir(ScoutDir::Down);
        for _ in 0..(Y_LEAD_IN_FRAMES - 1) {
            assert_eq!(r.observe(&p, 0, 100), None);
        }
        assert_eq!(r.observe(&p, 0, 100), Some(ScoutEnd::Stalled));
        // Once it has grown the ordinary rule applies.
        let mut r = StopRule::for_dir(ScoutDir::Down);
        assert_eq!(r.observe(&p, 0, 100), None);
        assert_eq!(r.observe(&p, 0, 200), None);
        assert_eq!(r.observe(&p, 0, 200), None);
        assert_eq!(r.observe(&p, 0, 200), None);
        assert_eq!(r.observe(&p, 0, 200), Some(ScoutEnd::Stalled));
    }

    #[test]
    fn player_move_pokes_the_player_word_not_the_camera() {
        let p = plan(ScoutStrategy::PlayerMove).pokes(ScoutDir::Right, 0x0100, 0x0300);
        assert_eq!(p[..2], [(0x7E_0094, 0x04), (0x7E_0095, 0x03)]);
        assert!(p.iter().all(|&(a, _)| a != 0x7E_001A));
    }

    #[test]
    fn a_sidestep_zigzags_up_then_down_with_forward_runs_between_and_ends() {
        let right = ScoutDir::Right.nes_pad_bit();
        let (up, down) = (ScoutDir::Up.nes_pad_bit(), ScoutDir::Down.nes_pad_bit());
        // Sidestep 0: UNIT frames up, then FORWARD frames the way the pass goes.
        assert_eq!(sidestep_pad(0, ScoutDir::Right), Some(up));
        assert_eq!(sidestep_pad(SIDESTEP_UNIT - 1, ScoutDir::Right), Some(up));
        assert_eq!(sidestep_pad(SIDESTEP_UNIT, ScoutDir::Right), Some(right));
        // Sidestep 1 is down, twice as long.
        let k1 = SIDESTEP_UNIT + SIDESTEP_FORWARD;
        assert_eq!(sidestep_pad(k1, ScoutDir::Right), Some(down));
        assert_eq!(
            sidestep_pad(k1 + 2 * SIDESTEP_UNIT, ScoutDir::Right),
            Some(right)
        );
        // Spent after SIDESTEPS of them, and the total is bounded (law 8: no index to stall).
        let total: u32 = (0..SIDESTEPS)
            .map(|k| SIDESTEP_UNIT * (k + 1) + SIDESTEP_FORWARD)
            .sum();
        assert_eq!(
            sidestep_pad(total - 1, ScoutDir::Left),
            Some(ScoutDir::Left.nes_pad_bit())
        );
        assert_eq!(sidestep_pad(total, ScoutDir::Left), None);
        assert_eq!(sidestep_pad(u32::MAX, ScoutDir::Left), None);
    }

    #[test]
    fn a_stall_waits_while_the_walker_sidesteps_and_the_budget_still_ends_the_pass() {
        let plan = plan(ScoutStrategy::Walk);
        let mut rule = StopRule::for_dir(ScoutDir::Right);
        rule.defer_stall = true;
        for _ in 0..plan.stall_frames + 5 {
            assert_eq!(rule.observe(&plan, 0, 10), None);
        }
        rule.defer_stall = false;
        assert_eq!(rule.observe(&plan, 0, 10), Some(ScoutEnd::Stalled));
        let mut rule = StopRule::for_dir(ScoutDir::Right);
        rule.defer_stall = true;
        let ended = (0..=plan.frame_budget).find_map(|_| rule.observe(&plan, 0, 10));
        assert_eq!(ended, Some(ScoutEnd::Budget));
    }

    #[test]
    fn walking_left_stops_at_zero_instead_of_wrapping() {
        let p = plan(ScoutStrategy::PlayerMove).pokes(ScoutDir::Left, 0, 2);
        assert_eq!(p[..2], [(0x7E_0094, 0), (0x7E_0095, 0)]);
    }

    #[test]
    fn a_pass_stalls_when_the_canvases_stop_growing() {
        let pl = plan(ScoutStrategy::PlayerMove);
        let mut r = StopRule::default();
        assert_eq!(r.observe(&pl, 10, 100), None);
        assert_eq!(r.observe(&pl, 14, 200), None);
        assert_eq!(r.observe(&pl, 18, 200), None);
        assert_eq!(r.observe(&pl, 18, 200), None);
        assert_eq!(r.observe(&pl, 18, 200), Some(ScoutEnd::Stalled));
    }

    #[test]
    fn growth_resets_the_stall_counter_and_the_budget_still_ends_the_pass() {
        let pl = plan(ScoutStrategy::PlayerMove);
        let mut r = StopRule::default();
        let mut last = None;
        for i in 0..20u32 {
            last = r.observe(&pl, i64::from(i * 4), 100 + i as usize);
            if last.is_some() {
                assert_eq!(i + 1, pl.frame_budget);
                break;
            }
        }
        assert_eq!(last, Some(ScoutEnd::Budget));
    }

    #[test]
    fn a_camera_jump_is_a_scene_change() {
        let pl = plan(ScoutStrategy::PlayerMove);
        let mut r = StopRule::default();
        assert_eq!(r.observe(&pl, 500, 1), None);
        assert_eq!(r.observe(&pl, 504, 2), None);
        assert_eq!(r.observe(&pl, 0, 3), Some(ScoutEnd::SceneChanged));
    }

    // ---- the scout on the RF-Scroller NES fixture (no ROM needed beyond the
    // fixture the repo builds): extends the canvas past what the live run saw.

    const RIGHT: u16 = 0x80;
    const NES_PLAYER_X: u32 = 0x6029; // FORMAT.md "Documented RAM addresses"
    const NES_CAMERA_X: u32 = NES_PLAYER_X + 2;

    fn fixture() -> Option<Vec<u8>> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/nes/rf-scroller/build/rf-scroller.nes");
        match std::fs::read(path) {
            Ok(b) => Some(b),
            Err(_) => {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "fixture ROM missing on CI: this test would pass having asserted nothing"
                );
                eprintln!("SKIP: fixture ROM not built");
                None
            }
        }
    }

    /// The NES has no layer registers: the scout places it by the core's own
    /// scroll events and never calls the reader.
    fn no_scrolls(_: &EmuStepper) -> Option<[Option<LayerScroll>; LAYERS]> {
        None
    }

    fn nes_sink() -> ScoutSink {
        ScoutSink {
            bundle: FrameBundleBuilder::new(256, 240),
            keep_events: true,
        }
    }

    /// One live frame, fed to the model the way the core thread does.
    fn live_frame(
        st: &mut EmuStepper,
        sink: &mut ScoutSink,
        live: &mut CanvasModel,
        pad: u16,
    ) -> SceneId {
        let mut input = InputFrame::empty();
        input.ports[0] = pad;
        st.latch_and_advance_frame(input, sink);
        let bundle = sink.bundle.take(st.frame_count());
        live.observe_frame(&bundle, &FrameFeed::bands())
    }

    fn plan_of(step: u16, budget: u32, stall: u32, settle: u32) -> ScoutPlan {
        ScoutPlan {
            strategy: ScoutStrategy::PlayerMove,
            camera: vec![NES_CAMERA_X],
            player_x: vec![NES_PLAYER_X],
            player_x_hi: Vec::new(),
            camera_y: Vec::new(),
            player_y: Vec::new(),
            player_y_hi: Vec::new(),
            step_px: step,
            frame_budget: budget,
            stall_frames: stall,
            settle_frames: settle,
            dirs: vec![ScoutDir::Right],
            hold: Vec::new(),
            start_when: Vec::new(),
            pin: false,
            scene_key: None,
            reach_px: 0,
        }
    }

    fn live_view<'a>(
        st: &'a EmuStepper,
        rom: &'a [u8],
        live: &'a mut CanvasModel,
        scene: SceneId,
    ) -> LiveView<'a> {
        LiveView {
            stepper: st,
            rom,
            canvases: live,
            scene,
            size: (256, 240),
            scrolls: no_scrolls,
            blank: false,
        }
    }

    #[test]
    fn rows_on_one_address_are_alternatives_and_addresses_all_hold() {
        let rows = [(0x12, 5), (0x12, 7), (0x6F, 4)];
        let at = |a: u32, b: u32| move |addr: u32| if addr == 0x12 { a as u8 } else { b as u8 };
        assert!(start_gate_open(&rows, at(5, 4)));
        assert!(start_gate_open(&rows, at(7, 4)));
        assert!(
            !start_gate_open(&rows, at(3, 4)),
            "no value of 0x12 matches"
        );
        assert!(!start_gate_open(&rows, at(5, 0)), "0x6F must hold as well");
        assert!(start_gate_open(&[], at(0, 0)));
    }

    #[test]
    fn a_picture_of_one_colour_is_flat() {
        let px = |i: u8| PpuPixel {
            palette_index: i,
            layer: rf_core_api::PixelLayer::Backdrop,
            sprite_id: None,
            priority: 0,
        };
        assert!(picture_is_flat(&[]));
        assert!(picture_is_flat(&[px(3), px(3), px(3)]));
        assert!(!picture_is_flat(&[px(3), px(3), px(4)]));
    }

    #[test]
    fn a_blank_picture_never_starts_a_scout() {
        let Some(rom) = fixture() else { return };
        let mut st = EmuStepper::open(&rom).expect("opens");
        let mut sink = nes_sink();
        let mut live = CanvasModel::new();
        let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
        let mut driver = ScoutDriver::default();
        driver.set(Some(ScoutSetup {
            plan: plan_of(2, 600, 40, 0),
            telemetry: telemetry.clone(),
        }));
        // Bounded: 200 live frames on a blank picture, then 200 on a real one.
        for f in 0..400u32 {
            let scene = live_frame(
                &mut st,
                &mut sink,
                &mut live,
                if f >= 60 { RIGHT } else { 0 },
            );
            let mut view = live_view(&st, &rom, &mut live, scene);
            view.blank = f < 200;
            driver.tick(view);
            if f < 200 {
                assert!(!driver.running(), "started on a blank picture (frame {f})");
            }
        }
        assert!(driver.running() || ScoutTelemetry::get(&telemetry.started) >= 1);
    }

    #[test]
    fn a_driver_waits_out_the_settle_frames_then_scouts_the_scene_once() {
        let Some(rom) = fixture() else { return };
        let mut st = EmuStepper::open(&rom).expect("opens");
        let mut sink = nes_sink();
        let mut live = CanvasModel::new();
        let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
        let mut driver = ScoutDriver::default();
        driver.set(Some(ScoutSetup {
            plan: plan_of(2, 600, 40, 90),
            telemetry: telemetry.clone(),
        }));
        let mut first_run_tick = None;
        let mut scene = SceneId(0);
        // Bounded: 300 live frames.
        for f in 0..300u32 {
            scene = live_frame(
                &mut st,
                &mut sink,
                &mut live,
                if f >= 60 { RIGHT } else { 0 },
            );
            driver.tick(live_view(&st, &rom, &mut live, scene));
            if first_run_tick.is_none() && driver.running() {
                first_run_tick = Some(f);
            }
        }
        // The worker runs beside the live loop: keep ticking (the machine
        // stands still, the scene is the same) until its deltas are merged.
        // Bounded: 3000 x 10 ms.
        for _ in 0..3_000u32 {
            if ScoutTelemetry::get(&telemetry.finished) >= 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            driver.tick(live_view(&st, &rom, &mut live, scene));
        }
        let first = first_run_tick.expect("a scout started");
        assert!(
            first >= 90,
            "settled for 90 frames first (started at {first})"
        );
        assert_eq!(ScoutTelemetry::get(&telemetry.started), 1, "once per scene");
        assert_eq!(ScoutTelemetry::get(&telemetry.finished), 1);
        assert!(ScoutTelemetry::get(&telemetry.cells) > 0);
        assert!(!driver.running());
    }

    /// Ticket W33-05: a scene whose scout finished is written with that fact,
    /// and the next session, over the same cache and the same frames, starts
    /// no scout at all.
    #[test]
    fn a_scene_restored_as_scouted_is_not_scouted_again() {
        let Some(rom) = fixture() else { return };
        let root = std::env::temp_dir().join(format!("rf-scout-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let session = |ticks_after: u32| {
            let cache = rf_cache::Cache::open(&root, 64 << 20).expect("cache");
            let mut st = EmuStepper::open(&rom).expect("opens");
            let mut sink = nes_sink();
            let mut live = CanvasModel::with_cache("rom-scroller".to_string(), Some(cache));
            let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
            let mut driver = ScoutDriver::default();
            driver.set(Some(ScoutSetup {
                plan: plan_of(2, 600, 40, 90),
                telemetry: telemetry.clone(),
            }));
            let mut scene = SceneId(0);
            // Bounded: 300 live frames.
            for f in 0..300u32 {
                scene = live_frame(
                    &mut st,
                    &mut sink,
                    &mut live,
                    if f >= 60 { RIGHT } else { 0 },
                );
                driver.tick(live_view(&st, &rom, &mut live, scene));
            }
            // Bounded: `ticks_after` x 10 ms, until a scout has finished.
            for _ in 0..ticks_after {
                if ScoutTelemetry::get(&telemetry.finished) >= 1 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                driver.tick(live_view(&st, &rom, &mut live, scene));
            }
            let tagged = live
                .scene_canvases(scene.0)
                .map_or(0, |c| c.iter().map(|c| c.scout_cell_count()).sum::<usize>());
            (
                telemetry,
                scene,
                live.scene_scouted(scene.0),
                live.scene_restored(scene.0),
                (live.scene_scout_memo(scene.0), tagged),
            )
        };
        let (first, scene, scouted, restored, (memo, tagged)) = session(3_000);
        assert_eq!(ScoutTelemetry::get(&first.started), 1);
        assert_eq!(ScoutTelemetry::get(&first.finished), 1);
        assert!(scouted && !restored, "the first visit is cold");
        // The model was dropped at the end of `session`: written.
        assert!(
            tagged > 0 && memo.is_some(),
            "the scout tagged its cells and kept its rung"
        );
        let (second, scene2, scouted2, restored2, (memo2, tagged2)) = session(0);
        assert_eq!(
            (memo2, tagged2),
            (memo, tagged),
            "both round-trip through the cache"
        );
        assert_eq!(scene2, scene, "same frames, same scene");
        assert!(restored2 && scouted2, "restored with its scout");
        assert_eq!(ScoutTelemetry::get(&second.started), 0, "no scout started");
        assert_eq!(ScoutTelemetry::get(&second.skipped), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_scene_change_cancels_the_worker_and_widescreen_off_does_too() {
        let Some(rom) = fixture() else { return };
        let mut st = EmuStepper::open(&rom).expect("opens");
        let mut sink = nes_sink();
        let mut live = CanvasModel::new();
        let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
        // Long enough that only cancelling can stop it soon.
        let setup = ScoutSetup {
            plan: plan_of(1, 100_000, 100_000, 0),
            telemetry: telemetry.clone(),
        };
        let mut driver = ScoutDriver::default();
        driver.set(Some(setup.clone()));
        let mut scene = SceneId(0);
        // Bounded: 200 live frames.
        for _ in 0..200u32 {
            scene = live_frame(&mut st, &mut sink, &mut live, 0);
            driver.tick(live_view(&st, &rom, &mut live, scene));
            if driver.running() {
                break;
            }
        }
        assert!(driver.running(), "a worker started");
        // The worker runs on its own thread: its frames count up with no
        // further ticks.
        let frames = || ScoutTelemetry::get(&telemetry.frames);
        let stopped = |what: &str| {
            // Bounded: 5 s. The worker reports its frames in batches, so
            // wait until two looks 150 ms apart agree.
            for _ in 0..100u32 {
                let a = frames();
                std::thread::sleep(std::time::Duration::from_millis(150));
                if frames() == a {
                    return;
                }
            }
            panic!("the worker kept running after {what}");
        };
        // A new scene: the old worker is dropped (cancelled).
        let other = SceneId(scene.0 ^ 1);
        driver.tick(live_view(&st, &rom, &mut live, other));
        assert!(!driver.running(), "the new scene starts unscouted");
        stopped("a scene change");
        // Disarming also cancels (what turning widescreen off does).
        driver.set(Some(setup));
        // The scene is forgotten, so it is scouted afresh.
        for _ in 0..200u32 {
            scene = live_frame(&mut st, &mut sink, &mut live, 0);
            driver.tick(live_view(&st, &rom, &mut live, scene));
            if driver.running() {
                break;
            }
        }
        assert!(driver.running(), "a second worker started");
        driver.set(None);
        assert!(!driver.running());
        stopped("being disarmed");
    }

    #[test]
    fn the_smw_profile_scout_table_loads_into_the_observed_plan() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles/snes/super-mario-world/profile.toml");
        let outcome = rf_profiles::load_file(&path).expect("the SMW profile loads");
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        let plan = ScoutPlan::from_profile(outcome.profile.scout.as_ref().expect("[scout]"));
        assert_eq!(plan.strategy, ScoutStrategy::PlayerMove);
        assert_eq!(plan.camera, vec![0x7E_001A]);
        assert_eq!(plan.player_x, vec![0x7E_0094]);
        assert_eq!(plan.hold, vec![(0x7E_0096, 150), (0x7E_0097, 0)]);
        assert_eq!(plan.start_when, vec![(0x7E_0100, 0x14), (0x7E_0071, 0)]);
        assert_eq!(plan.dirs, vec![ScoutDir::Right, ScoutDir::Left]);
        assert_eq!(
            (plan.step_px, plan.frame_budget, plan.stall_frames),
            (8, 1500, 60)
        );
    }

    #[test]
    fn a_driver_does_not_start_while_the_live_game_is_not_ready() {
        let Some(rom) = fixture() else { return };
        let mut st = EmuStepper::open(&rom).expect("opens");
        let mut sink = nes_sink();
        let mut live = CanvasModel::new();
        let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
        let mut driver = ScoutDriver::default();
        // A byte that never holds this value: the game is never "ready".
        let mut plan = plan_of(4, 100, 20, 0);
        plan.start_when = vec![(NES_PLAYER_X, 0xEE)];
        driver.set(Some(ScoutSetup {
            plan,
            telemetry: telemetry.clone(),
        }));
        // Bounded: 150 live frames.
        for _ in 0..150u32 {
            let scene = live_frame(&mut st, &mut sink, &mut live, 0);
            driver.tick(live_view(&st, &rom, &mut live, scene));
        }
        assert_eq!(ScoutTelemetry::get(&telemetry.started), 0);
        assert!(!driver.running());
    }

    // ---- W32-09: a NES scout on the RF-Scroller fixture.

    /// Run `frames` live frames of RF-Scroller (Right held from frame 60),
    /// feeding the model the way the core thread does.
    fn nes_live(rom: &[u8], frames: u32) -> (EmuStepper, CanvasModel, SceneId) {
        let mut st = EmuStepper::open(rom).expect("fixture opens");
        let mut sink = nes_sink();
        let mut live = CanvasModel::new();
        let mut scene = SceneId(0);
        for f in 0..frames {
            scene = live_frame(
                &mut st,
                &mut sink,
                &mut live,
                if f >= 60 { RIGHT } else { 0 },
            );
        }
        (st, live, scene)
    }

    fn nes_plan() -> ScoutPlan {
        ScoutPlan {
            camera: Vec::new(),
            ..plan_of(4, 400, 60, 0)
        }
    }

    #[test]
    fn a_nes_scout_on_rf_scroller_extends_the_canvas_and_leaves_live_cells_and_the_hud_alone() {
        let Some(rom) = fixture() else { return };
        let (live_st, mut live, scene) = nes_live(&rom, 200);
        let before = live.scene_canvases(scene.0).expect("live canvas")[0].clone();
        let cam_now = word(&live_st, NES_CAMERA_X);
        let snapshot = live_st.snapshot().expect("snapshot");
        let mut scout =
            Scout::start(&rom, snapshot, nes_plan(), &live, scene, (256, 240)).expect("starts");
        // Law 8: bounded; the plan's own budget ends the pass well before.
        for _ in 0..2_000 {
            if scout.step(&no_scrolls) != ScoutStatus::Running {
                break;
            }
        }
        assert!(
            matches!(scout.status(), ScoutStatus::Finished(_)),
            "ended: {:?}",
            scout.ends
        );
        let origin_y = scout.frames().and_then(|f| f.origins[0]).map(|o| o.1);
        let delta = scout.take_delta().expect("a delta");
        // The HUD band (the fixture's two top rows of tiles) is never stitched
        // by the scout: the cells of those rows, past the live canvas, are
        // unknown, while the playfield rows below are known.
        let oy = origin_y.expect("the last frame was placed");
        let far = before.origin().0 + before.width() as i64 + 40;
        let known = |y: i64| (far..far + 200).any(|x| delta[0].get(x, y).is_some());
        assert!(known(oy + 100), "the scout stitched the playfield");
        assert!(!known(oy + 4), "the HUD band was left out");
        let merged = live.merge_unknown_from(scene.0, &delta);
        assert!(merged > 0, "the scout found unknown cells");
        let after = &live.scene_canvases(scene.0).unwrap()[0];
        assert!(
            after.width() > before.width(),
            "canvas {} -> {} wide (camera was at {cam_now})",
            before.width(),
            after.width()
        );
        // Live always wins: every cell the live run knew is unchanged.
        let (ox, oy) = before.origin();
        for y in 0..before.height() as i64 {
            for x in 0..before.width() as i64 {
                if let Some(p) = before.get(ox + x, oy + y) {
                    assert_eq!(after.get(ox + x, oy + y), Some(p), "cell ({x},{y}) changed");
                }
            }
        }
        // The live machine was only read.
        assert_eq!(word(&live_st, NES_CAMERA_X), cam_now);
    }

    /// W35-08: a scout started from where another stands carries that one's scene tracking, so
    /// the new place is not a cut on its first frame, and its Left pass scrolls back over ground
    /// the first one crossed.
    #[test]
    fn a_scout_started_from_here_scouts_left_from_where_the_right_pass_ended() {
        let Some(rom) = fixture() else { return };
        let (live_st, live, scene) = nes_live(&rom, 200);
        let snapshot = live_st.snapshot().expect("snapshot");
        let mut scout =
            Scout::start(&rom, snapshot, nes_plan(), &live, scene, (256, 240)).expect("starts");
        // Law 8: bounded; the plan's own budget ends the pass well before.
        for _ in 0..2_000 {
            if scout.step(&no_scrolls) != ScoutStatus::Running {
                break;
            }
        }
        let mut left_plan = nes_plan();
        left_plan.dirs = vec![ScoutDir::Left];
        let mut back = scout
            .start_from_here(&rom, left_plan, (256, 240))
            .expect("starts from here");
        for _ in 0..2_000 {
            if back.step(&no_scrolls) != ScoutStatus::Running {
                break;
            }
        }
        // A model that did not know the place would call its first frame a cut (frame 1); the
        // fixture's world wraps, so the pass may still end in a cut later.
        assert!(
            back.frames_run() > 30,
            "the new place was taken for a cut: {:?} after {} frames",
            back.ends,
            back.frames_run()
        );
    }

    #[test]
    fn the_walk_strategy_pokes_only_the_hold_table() {
        let mut plan = nes_plan();
        plan.strategy = ScoutStrategy::Walk;
        plan.hold = vec![(0x04A0, 0x20)];
        assert_eq!(plan.pokes(ScoutDir::Right, 0, 0), vec![(0x04A0, 0x20)]);
    }

    #[test]
    fn a_player_x_hi_row_splits_the_poke_across_two_addresses() {
        let mut plan = nes_plan();
        plan.player_x = vec![0x0090];
        plan.player_x_hi = vec![0x0075];
        assert_eq!(
            plan.pokes(ScoutDir::Right, 0, 0x01FE),
            vec![(0x0090, 0x02), (0x0075, 0x02)]
        );
    }

    fn profile_plan_of(rel: &str) -> ScoutPlan {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        let outcome = rf_profiles::load_file(&path).expect("the profile loads");
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        ScoutPlan::from_profile(outcome.profile.scout.as_ref().expect("[scout]"))
    }

    #[test]
    fn the_smb3_profile_scout_table_loads_into_the_observed_plan() {
        let plan = profile_plan_of("../../profiles/nes/super-mario-bros-3/profile.toml");
        assert_eq!(plan.strategy, ScoutStrategy::PlayerMove);
        assert_eq!(
            (plan.player_x.clone(), plan.player_x_hi.clone()),
            (vec![0x90], vec![0x75])
        );
        assert_eq!(plan.hold, vec![(0x00A2, 0x28)]);
        assert_eq!(plan.start_when, vec![(0x000F, 1)]);
        assert_eq!(
            (plan.step_px, plan.frame_budget, plan.stall_frames),
            (4, 1500, 60)
        );
    }

    #[test]
    fn the_mm2_profile_scout_table_loads_into_the_observed_plan() {
        let plan = profile_plan_of("../../profiles/nes/mega-man-2/profile.toml");
        assert_eq!(plan.strategy, ScoutStrategy::Walk);
        assert_eq!(plan.hold, vec![(0x04A0, 0x20)]);
        // W35-11's gate: $36D is 0x38 and $2A is 1 or 2 (alternatives on one address)
        assert_eq!(
            plan.start_when,
            vec![(0x036D, 0x38), (0x002A, 1), (0x002A, 2)]
        );
        // W35-12: left as well (the Left pass ends at once from a stage's start)
        assert_eq!(plan.dirs, vec![ScoutDir::Right, ScoutDir::Left]);
        assert_eq!(plan.frame_budget, 3000);
    }

    // ---- W33-06: the worker's self-check and the strategy ladder, on the
    // RF-Scroller fixture (a scroller that cannot keep up with a 4 px step).

    struct Ran {
        deltas: usize,
        delta_cells: usize,
        done: Option<(Vec<ScoutEnd>, u64, ScoutReport)>,
        failed: bool,
        telemetry: std::sync::Arc<ScoutTelemetry>,
    }

    /// Run a worker to its end on a thread and collect what it sends.
    fn run_job(
        rom: &[u8],
        live: &CanvasModel,
        st: &EmuStepper,
        scene: SceneId,
        plan: ScoutPlan,
        hint: Option<(ScoutStrategy, u16)>,
        poison: impl FnOnce(&mut ScoutReference),
    ) -> Ran {
        let mut reference = live.scout_reference(scene.0).expect("a live canvas");
        poison(&mut reference);
        let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
        let job = ScoutJob {
            rom: rom.to_vec(),
            snapshot: st.snapshot().expect("snapshot"),
            plan,
            seed: live.fork_for_scout(),
            reference: Some(reference),
            hint,
            scene,
            size: (256, 240),
            reader: no_scrolls,
            telemetry: telemetry.clone(),
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(CHANNEL_DEPTH);
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c2 = cancel.clone();
        let worker = std::thread::spawn(move || run_worker(job, &tx, &c2));
        let mut ran = Ran {
            deltas: 0,
            delta_cells: 0,
            done: None,
            failed: false,
            telemetry,
        };
        // Bounded: the worker ends (its budgets) and drops the sender.
        while let Ok(msg) = rx.recv() {
            match msg {
                ScoutMsg::Delta { canvases, .. } => {
                    ran.deltas += 1;
                    ran.delta_cells += canvases
                        .iter()
                        .map(|c| c.cells().iter().flatten().count())
                        .sum::<usize>();
                }
                ScoutMsg::Done {
                    ends,
                    frames,
                    report,
                } => ran.done = Some((ends, frames, *report)),
                ScoutMsg::Failed => ran.failed = true,
            }
        }
        worker.join().expect("the worker ends");
        ran
    }

    /// Every known cell of layer 0 recoloured: the live canvas the scout is
    /// checked against no longer looks like the picture the scout draws.
    fn recolour(r: &mut ScoutReference) {
        let c = &r.layers[0];
        let (ox, oy) = c.origin();
        let mut out = Canvas::new();
        for y in 0..c.height() {
            let row: Vec<_> = (0..c.width())
                .map(|x| {
                    c.get(ox + x as i64, oy + y as i64).map(|mut p| {
                        p.palette_index = p.palette_index.wrapping_add(1);
                        p
                    })
                })
                .collect();
            out.blit_row(ox, oy + y as i64, &row);
        }
        r.layers[0] = out;
    }

    fn attempts_of(
        r: &ScoutReport,
    ) -> Vec<(&'static str, ScoutStrategy, u16, Option<ScoutVerdict>)> {
        r.attempts
            .iter()
            .map(|a| (a.what, a.strategy, a.step_px, a.check.verdict))
            .collect()
    }

    #[test]
    fn a_step_the_game_cannot_keep_up_with_is_rejected_and_the_scout_is_retried_at_half_of_it() {
        let Some(rom) = fixture() else { return };
        let (st, live, scene) = nes_live(&rom, 300);
        let ran = run_job(
            &rom,
            &live,
            &st,
            scene,
            plan_of(4, 600, 40, 0),
            None,
            |_| {},
        );
        let (_, _, report) = ran.done.expect("the scout finished");
        let a = attempts_of(&report);
        // The 4 px step disagrees with the same stretch at 2 px: refused
        // before a single cell leaves the worker, then 2 px is accepted.
        assert_eq!(
            a[0],
            (
                "probe",
                ScoutStrategy::PlayerMove,
                4,
                Some(ScoutVerdict::Reject)
            ),
            "{a:?}"
        );
        assert_eq!(a[1].1, ScoutStrategy::PlayerMove);
        assert_eq!((a[1].0, a[1].2), ("delta", 2), "{a:?}");
        // W35-07: the fixture's frames where its vblank work runs into the picture (FORMAT.md
        // "Known defects") now sit on the canvas where they are drawn, and the 2 px scout agrees
        // with the live canvas on 92% of the cells it shares (96% while a mid-frame `$2005` Y
        // put the playfield 15 rows high): under the 95% bar. The ladder leaves player_move for
        // walk, which agrees on every shared cell.
        assert_eq!(a[1].3, Some(ScoutVerdict::Reject), "{a:?}");
        assert_eq!(a[2].1, ScoutStrategy::Walk, "{a:?}");
        assert!(
            matches!(a[2].3, Some(ScoutVerdict::Merge | ScoutVerdict::SlowDown)),
            "accepted, whichever of the two: {a:?}"
        );
        assert_eq!(report.winner().map(|w| w.0), Some(ScoutStrategy::Walk));
        assert!(!report.gave_up);
        assert!(
            ran.deltas > 0 && ran.delta_cells > 0,
            "the accepted pass shipped cells"
        );
        let t = &ran.telemetry;
        assert!(ScoutTelemetry::get(&t.probes_rejected) >= 1);
        assert!(ScoutTelemetry::get(&t.deltas_merged) + ScoutTelemetry::get(&t.deltas_slow) >= 1);
        assert!(
            t.agreement_permille().is_some_and(|p| p >= 700),
            "agreement is reported"
        );
        assert!(t.shared_cells.load(std::sync::atomic::Ordering::Relaxed) > 2000);
    }

    #[test]
    fn a_pass_the_live_canvas_contradicts_is_never_merged_and_every_rung_is_tried_in_order() {
        let Some(rom) = fixture() else { return };
        let (st, live, scene) = nes_live(&rom, 300);
        let ran = run_job(
            &rom,
            &live,
            &st,
            scene,
            plan_of(4, 600, 40, 0),
            None,
            recolour,
        );
        assert_eq!(
            ran.deltas, 0,
            "a rejected delta is never sent, so never merged"
        );
        let (_, _, report) = ran.done.expect("it ends, having given up");
        assert!(report.gave_up && report.winner().is_none());
        let deltas: Vec<_> = report
            .attempts
            .iter()
            .filter(|a| a.what == "delta")
            .map(|a| (a.strategy, a.step_px, a.check.verdict))
            .collect();
        assert!(
            deltas.iter().all(|d| d.2 == Some(ScoutVerdict::Reject)),
            "{deltas:?}"
        );
        // player_move (down to its floor), then walk, then camera_write.
        let mut order: Vec<ScoutStrategy> = Vec::new();
        for d in &deltas {
            if order.last() != Some(&d.0) {
                order.push(d.0);
            }
        }
        assert_eq!(
            order,
            vec![
                ScoutStrategy::PlayerMove,
                ScoutStrategy::Walk,
                ScoutStrategy::CameraWrite
            ],
            "{deltas:?}"
        );
        assert_eq!(ScoutTelemetry::get(&ran.telemetry.deltas_merged), 0);
        assert!(ScoutTelemetry::get(&ran.telemetry.deltas_rejected) >= 3);
        assert!(
            report.summary().contains("-> gave up"),
            "{}",
            report.summary()
        );
    }

    #[test]
    fn a_scene_the_live_game_has_not_drawn_cannot_judge_a_scout_and_is_not_refused_for_it() {
        let Some(rom) = fixture() else { return };
        let (st, live, scene) = nes_live(&rom, 300);
        // The player stood still at the start of the level: the live canvas
        // holds nothing to compare with.
        let ran = run_job(&rom, &live, &st, scene, plan_of(4, 600, 40, 0), None, |r| {
            r.layers = Default::default();
        });
        let (_, _, report) = ran.done.expect("done");
        assert_eq!(report.reference_cells, 0);
        assert!(!report.gave_up, "{}", report.summary());
        // The probe still refuses the step the game cannot keep up with; the
        // pass at the half step goes through with no live verdict.
        let a = attempts_of(&report);
        assert_eq!(
            a[0],
            (
                "probe",
                ScoutStrategy::PlayerMove,
                4,
                Some(ScoutVerdict::Reject)
            ),
            "{a:?}"
        );
        assert_eq!(report.winner(), Some((ScoutStrategy::PlayerMove, 2)));
        assert!(ran.deltas > 0 && ran.delta_cells > 0, "the cells were sent");
        assert!(ScoutTelemetry::get(&ran.telemetry.deltas_unjudged) > 0);
        assert_eq!(ScoutTelemetry::get(&ran.telemetry.deltas_rejected), 0);
    }

    #[test]
    fn a_pinned_profile_gives_up_without_trying_another_rung() {
        let Some(rom) = fixture() else { return };
        let (st, live, scene) = nes_live(&rom, 300);
        let mut plan = plan_of(2, 600, 40, 0);
        plan.pin = true;
        let ran = run_job(&rom, &live, &st, scene, plan, None, recolour);
        let (_, _, report) = ran.done.expect("done");
        assert!(report.gave_up);
        assert!(
            report
                .attempts
                .iter()
                .all(|a| a.strategy == ScoutStrategy::PlayerMove),
            "{:?}",
            attempts_of(&report)
        );
    }

    #[test]
    fn the_rung_remembered_for_the_scene_is_tried_first() {
        let Some(rom) = fixture() else { return };
        let (st, live, scene) = nes_live(&rom, 300);
        let ran = run_job(
            &rom,
            &live,
            &st,
            scene,
            plan_of(2, 600, 40, 0),
            Some((ScoutStrategy::CameraWrite, 2)),
            recolour,
        );
        let (_, _, report) = ran.done.expect("done");
        assert_eq!(report.attempts[0].strategy, ScoutStrategy::CameraWrite);
    }

    #[test]
    fn an_accepted_scout_remembers_its_rung_with_the_scene_and_a_give_up_is_remembered_too() {
        let Some(rom) = fixture() else { return };
        let (st, mut live, scene) = nes_live(&rom, 300);
        let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
        let mut driver = ScoutDriver::default();
        driver.set(Some(ScoutSetup {
            plan: plan_of(2, 600, 40, 0),
            telemetry: telemetry.clone(),
        }));
        // Bounded: 3000 x 10 ms.
        for _ in 0..3_000u32 {
            driver.tick(live_view(&st, &rom, &mut live, scene));
            if ScoutTelemetry::get(&telemetry.finished) >= 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(ScoutTelemetry::get(&telemetry.finished), 1);
        let memo = live.scene_scout_memo(scene.0).expect("the rung was kept");
        // W35-07: the 2 px player_move scout of this fixture agrees with the live canvas on 92%
        // of the cells it shares now (see `a_step_the_game_cannot_keep_up_...`), so the rung
        // kept is walk's, as the summary says.
        assert_eq!(
            memo.rung,
            rf_enhance::persistence::ScoutMemo::WALK,
            "{:?}",
            telemetry.last_summary()
        );
        assert!(live.scene_scouted(scene.0));
        assert!(telemetry
            .last_summary()
            .is_some_and(|s| s.contains("walk@")));
        assert!(ScoutTelemetry::get(&telemetry.cells) > 0);
        // The cells it filled are Scout cells; none is a live one.
        let c = &live.scene_canvases(scene.0).unwrap()[0];
        assert!(c.scout_cell_count() > 0);
        assert_eq!(
            c.scout_cell_count() as u64,
            ScoutTelemetry::get(&telemetry.cells),
            "every merged cell is tagged"
        );
    }

    #[test]
    fn the_rules_for_a_delta_follow_the_plan() {
        use Disposition::*;
        use ScoutVerdict::{Merge, Reject, SlowDown};
        // (verdict, accepted, ended, empty, judgeable)
        let d = disposition;
        // The first verdict decides the pass.
        assert_eq!(
            d(Some(Merge), false, false, false, true),
            Send { lower: false }
        );
        assert_eq!(
            d(Some(SlowDown), false, false, false, true),
            Send { lower: true }
        );
        assert_eq!(d(Some(Reject), false, false, false, true), Refuse);
        // Afterwards a reject only drops that delta; a slow one is merged and
        // does not lower the step again.
        assert_eq!(d(Some(Reject), true, false, false, true), Drop);
        assert_eq!(
            d(Some(SlowDown), true, false, false, true),
            Send { lower: false }
        );
        // No verdict: held while the live canvas might still give one and the
        // pass runs; merged once the pass is accepted or has ended, or when
        // the live canvas could never judge it.
        assert_eq!(d(None, false, false, false, true), Hold);
        assert_eq!(d(None, true, false, false, true), SendUnjudged);
        assert_eq!(d(None, false, true, false, true), SendUnjudged);
        assert_eq!(d(None, false, false, false, false), SendUnjudged);
        // Nothing stitched: nothing to do, whatever else is true.
        assert_eq!(d(None, false, true, true, true), Nothing);
        assert_eq!(d(None, false, false, true, false), Nothing);
        // A rejected delta is never sent, whatever else is true.
        for accepted in [false, true] {
            for ended in [false, true] {
                for empty in [false, true] {
                    for judgeable in [false, true] {
                        let r = d(Some(Reject), accepted, ended, empty, judgeable);
                        assert!(matches!(r, Refuse | Drop), "{r:?}");
                    }
                }
            }
        }
    }
}
