//! Ticket W32-07: the level scout -- a second machine that draws the level
//! AHEAD of (and behind) the camera so the widescreen margins show real level
//! the player has not visited.
//!
//! The stitched canvases ([`crate::layer_canvas`]) only know what the camera
//! has seen. The scout is the same machine, restored from a snapshot of the
//! live one at the moment a scene is entered, then driven somewhere the
//! player has not been: each scout frame it pokes the game's own camera (or
//! the player's position, so the game's follow logic moves the camera and
//! streams the columns), steps one frame into a private sink and stitches the
//! picture with a [`LayerCanvasAccumulator`] whose world axes continue the
//! live ones, so scout cells land on the same world coordinates as live ones.
//! Its cells then fill only the UNKNOWN cells of the live canvases
//! ([`LayerCanvasAccumulator::merge_unknown_from`]): live always wins.
//!
//! **Law 4 (the live core is never touched).** The scout owns its own
//! [`EmuStepper`]; the live one is only ever read (`snapshot`). The scout's
//! machine shares no mutable state with the live one, its audio is dropped,
//! and its input is held neutral. Sprites are never taken from it (only
//! `Background(n)` pixels are stitched), because anything the scout's
//! player does to the game -- enemies waking, items spawning -- must not
//! appear in the live picture.
//!
//! Everything here runs on the core thread (the only thread that may touch a
//! machine); `Scout::step` runs one frame and returns, so the caller decides
//! how many to run per live frame (wall-time budget).
//!
//! The recipe (which RAM words, how far per frame) is per game and comes from
//! the profile's `[scout]` table; the pure rules (what to poke, when to stop)
//! are plain functions so they are tested without a ROM.

use rf_core_api::{CoreSink, FrameBundleBuilder, InputFrame, PpuPixel};
use rf_enhance::scene_identity::SceneId;

use crate::layer_canvas::{LayerCanvasAccumulator, LayerScroll, LAYERS};
use crate::stepper::{EmuStepper, MachineSnapshot};

/// How the scout moves the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoutStrategy {
    /// Write the camera X words directly each frame.
    CameraWrite,
    /// Move the player X words; the game's own follow logic moves the camera
    /// and streams the level.
    PlayerMove,
}

/// Which way a pass walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoutDir {
    Right,
    Left,
}

impl ScoutDir {
    const fn sign(self) -> i32 {
        match self {
            ScoutDir::Right => 1,
            ScoutDir::Left => -1,
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
    /// own restart timer).
    pub start_when: Vec<(u32, u8)>,
}

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
            || vec![ScoutDir::Right, ScoutDir::Left],
            |d| {
                d.iter()
                    .map(|s| {
                        if s == "left" {
                            ScoutDir::Left
                        } else {
                            ScoutDir::Right
                        }
                    })
                    .collect()
            },
        );
        Self {
            strategy: if t.strategy == "camera_write" {
                ScoutStrategy::CameraWrite
            } else {
                ScoutStrategy::PlayerMove
            },
            camera: words("camera_x"),
            player_x: words("player_x"),
            step_px: t.step_px,
            frame_budget: t.frame_budget,
            stall_frames: t.stall_frames.unwrap_or(120),
            settle_frames: t.settle_frames.unwrap_or(90),
            dirs,
            hold: valued("hold"),
            start_when: valued("start_when"),
        }
    }

    /// The camera moved by more than this in one frame is a scene change
    /// (a transition or a respawn), not scrolling.
    pub const JUMP_PX: i32 = 64;

    /// The pokes for one scout frame, given what the scout machine holds now.
    #[must_use]
    pub fn pokes(&self, dir: ScoutDir, camera: u16, player: u16) -> Vec<(u32, u8)> {
        let step = i32::from(self.step_px) * dir.sign();
        let (addrs, from) = match self.strategy {
            ScoutStrategy::CameraWrite => (&self.camera, camera),
            ScoutStrategy::PlayerMove => (&self.player_x, player),
        };
        let to = (i32::from(from) + step).clamp(0, i32::from(u16::MAX)) as u16;
        let mut out: Vec<(u32, u8)> = Vec::with_capacity(addrs.len() * 2 + self.hold.len());
        for &a in addrs {
            out.push((a, to as u8));
            out.push((a + 1, (to >> 8) as u8));
        }
        out.extend(self.hold.iter().copied());
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

/// The stop rules, as a small state machine over per-frame facts.
#[derive(Debug, Clone, Copy, Default)]
pub struct StopRule {
    frames: u32,
    still: u32,
    extent: usize,
    last_camera: Option<u16>,
}

impl StopRule {
    /// Whether `camera` is a jump from the last frame's (a scene change).
    #[must_use]
    pub fn jumped(&self, camera: u16) -> bool {
        self.last_camera
            .is_some_and(|prev| (i32::from(camera) - i32::from(prev)).abs() > ScoutPlan::JUMP_PX)
    }

    /// Feed one frame: the camera word read after it and the canvases'
    /// total extent (cells in their bounding boxes). `Some` ends the pass.
    pub fn observe(&mut self, plan: &ScoutPlan, camera: u16, extent: usize) -> Option<ScoutEnd> {
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
        if self.still >= plan.stall_frames {
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

/// Collects a frame as a bundle; audio and events are dropped (events are
/// not kept: the scout's stitcher reads scroll registers, not event logs).
struct ScoutSink {
    bundle: FrameBundleBuilder,
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
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: rf_core_api::CoreEvent) {}
}

/// Reads the per-layer scroll the stitcher needs off the scout's machine
/// (the same function the live path uses, so the two agree on layers).
pub type ScrollFn<'a> = &'a dyn Fn(&EmuStepper) -> Option<[Option<LayerScroll>; LAYERS]>;

pub struct Scout {
    stepper: EmuStepper,
    snapshot: MachineSnapshot,
    plan: ScoutPlan,
    scene: SceneId,
    /// The seed the live axes had at the snapshot (axes only, no canvases).
    seed: LayerCanvasAccumulator,
    acc: LayerCanvasAccumulator,
    sink: ScoutSink,
    pass: usize,
    rule: StopRule,
    status: ScoutStatus,
    frames_total: u64,
    since_merge: u32,
    /// The scroll the machine's registers hold right now: what draws its
    /// NEXT frame (see [`Self::step`]). `None` at the start of a pass.
    next_scrolls: Option<[Option<LayerScroll>; LAYERS]>,
    /// How the passes ended, in order.
    pub ends: Vec<ScoutEnd>,
}

impl Scout {
    /// Open a second machine of `rom` and put it where `snapshot` was taken.
    /// `live` is the live accumulator at that moment (only its world axes are
    /// copied); `size` is the live frame's `(width, height)`.
    ///
    /// # Errors
    /// The ROM cannot be opened, or the snapshot cannot be restored.
    pub fn start(
        rom: &[u8],
        snapshot: MachineSnapshot,
        plan: ScoutPlan,
        live: &LayerCanvasAccumulator,
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
        stepper: EmuStepper,
        snapshot: MachineSnapshot,
        plan: ScoutPlan,
        live: &LayerCanvasAccumulator,
        scene: SceneId,
        size: (u16, u16),
    ) -> Self {
        let seed = live.fork_axes();
        Self {
            stepper,
            snapshot,
            scene,
            acc: seed.fork_axes(),
            seed,
            sink: ScoutSink {
                bundle: FrameBundleBuilder::new(size.0, size.1),
            },
            pass: 0,
            rule: StopRule::default(),
            status: ScoutStatus::Running,
            frames_total: 0,
            since_merge: 0,
            next_scrolls: None,
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
            self.plan
                .player_x
                .first()
                .map_or(0, |&a| word(&self.stepper, a)),
        )
    }

    /// Read any work-RAM word on the scout's machine (diagnostics, tests).
    #[must_use]
    pub fn peek_word(&self, addr: u32) -> u16 {
        word(&self.stepper, addr)
    }

    /// The scout's own stitched canvases.
    #[must_use]
    pub fn accumulator(&self) -> &LayerCanvasAccumulator {
        &self.acc
    }

    /// Frames stitched since the last [`Self::merge_into`].
    #[must_use]
    pub fn frames_since_merge(&self) -> u32 {
        self.since_merge
    }

    /// Fill the unknown cells of `live` for this scout's scene; returns the
    /// number of cells filled. Live cells are never touched.
    pub fn merge_into(&mut self, live: &mut LayerCanvasAccumulator) -> usize {
        self.since_merge = 0;
        let dirty = self.acc.take_dirty();
        let Some(from) = self.acc.scene_canvases(self.scene.0) else {
            return 0;
        };
        live.merge_unknown_within(self.scene.0, from, &dirty)
    }

    fn extent(&self) -> usize {
        self.acc
            .scene_canvases(self.scene.0)
            .map_or(0, |set| set.iter().map(|c| c.width() * c.height()).sum())
    }

    fn end_pass(&mut self, why: ScoutEnd) {
        self.ends.push(why);
        self.pass += 1;
        if self.pass >= self.plan.dirs.len() {
            self.status = ScoutStatus::Finished(why);
            return;
        }
        // Next pass: back to the snapshot, axes back to the live seed (the
        // canvases built so far are kept).
        if self.stepper.restore(&self.snapshot).is_err() {
            self.status = ScoutStatus::Finished(why);
            return;
        }
        self.acc.rewind_axes(&self.seed);
        self.rule = StopRule::default();
        self.next_scrolls = None;
    }

    /// Run ONE scout frame: poke, step, stitch, apply the stop rules.
    /// `scrolls` reads the per-layer scroll off the scout's machine.
    pub fn step(&mut self, scrolls: ScrollFn<'_>) -> ScoutStatus {
        if let ScoutStatus::Finished(_) = self.status {
            return self.status;
        }
        let dir = self.plan.dirs[self.pass];
        // The scroll registers read AFTER a frame are the ones that draw the
        // NEXT frame (the game's NMI writes them in vblank, after the visible
        // lines): observed on Super Mario World, a frame stitched at the
        // value read after it lands exactly one frame of scroll too far
        // (W32-07 note). So a frame is stitched at the value read after the
        // one before it; at the start of a pass that is what the restored
        // registers hold now.
        let drawn_with = self.next_scrolls.take().or_else(|| scrolls(&self.stepper));
        let cam = self
            .plan
            .camera
            .first()
            .map_or(0, |&a| word(&self.stepper, a));
        let player = self
            .plan
            .player_x
            .first()
            .map_or(0, |&a| word(&self.stepper, a));
        for (addr, value) in self.plan.pokes(dir, cam, player) {
            self.stepper.poke_ram(addr, value);
        }
        // Neutral input: the scout's player does nothing of its own.
        self.stepper
            .latch_and_advance_frame(InputFrame::empty(), &mut self.sink);
        self.frames_total += 1;
        let after = self
            .plan
            .camera
            .first()
            .map_or(0, |&a| word(&self.stepper, a));
        // A jump ends the pass BEFORE this frame is stitched: it may already
        // be a different scene's picture.
        if self.rule.jumped(after) {
            drop(self.sink.bundle.take(self.stepper.frame_count()));
            self.end_pass(ScoutEnd::SceneChanged);
            return self.status;
        }
        let bundle = self.sink.bundle.take(self.stepper.frame_count());
        self.next_scrolls = scrolls(&self.stepper);
        self.acc
            .observe_frame(&bundle, self.scene, drawn_with.as_ref());
        self.since_merge += 1;
        let extent = self.extent();
        if let Some(why) = self.rule.observe(&self.plan, after, extent) {
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
    /// Scouts that ended almost at once (they began mid-transition) and were
    /// retried.
    pub aborted: std::sync::atomic::AtomicU64,
    /// Scout frames run, over all scouts.
    pub frames: std::sync::atomic::AtomicU64,
    /// Cells merged into the live canvases.
    pub cells: std::sync::atomic::AtomicU64,
    /// Wall time spent in scout slices, microseconds.
    pub wall_us: std::sync::atomic::AtomicU64,
    /// Slices whose first frame ran although it did not fit the budget (the
    /// loop was behind, or not paced): the starvation guard.
    pub forced: std::sync::atomic::AtomicU64,
    /// Microseconds by which budgeted (non-forced) slices overran their
    /// budget, summed. The budget exists to keep this near zero.
    pub over_us: std::sync::atomic::AtomicU64,
    /// The longest single slice, microseconds.
    pub max_slice_us: std::sync::atomic::AtomicU64,
    /// Live frames on which a slice ran.
    pub slices: std::sync::atomic::AtomicU64,
    /// How the passes of the most recent finished scout ended.
    pub last_ends: std::sync::Mutex<Vec<ScoutEnd>>,
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
}

/// A plan plus where to report to.
#[derive(Debug, Clone)]
pub struct ScoutSetup {
    pub plan: ScoutPlan,
    pub telemetry: std::sync::Arc<ScoutTelemetry>,
    /// Run [`MAX_FRAMES_PER_TICK`] scout frames per live frame, whatever the
    /// pacer's spare time says (tests that want the whole scout in a few
    /// live frames). Never set by the app.
    pub turbo: bool,
}

/// Most scout frames one live frame carries, whatever the budget says.
pub const MAX_FRAMES_PER_TICK: u32 = 16;
/// A starved scout gets one frame this often (see `ScoutDriver::run_slice`).
const STARVE_TICKS: u32 = 3;
/// The slice budget when the loop is not paced (fast-forward, tests).
pub const UNPACED_SLICE: std::time::Duration = std::time::Duration::from_millis(3);
/// Idle time left over for the pacer's own sleep and jitter.
pub const PACED_MARGIN: std::time::Duration = std::time::Duration::from_millis(3);
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
/// Scout frames between merges into the live canvases (each merge scans
/// the scout canvases once).
const MERGE_EVERY: u32 = 60;

/// Decides WHEN to scout and how much to run per live frame; owns the
/// running [`Scout`]. Driven once per live frame from the core thread.
#[derive(Default)]
pub struct ScoutDriver {
    setup: Option<ScoutSetup>,
    scene: Option<u64>,
    settle_left: u32,
    attempts: u32,
    /// This scene's scout has finished (or been given up): never again.
    done: bool,
    scout: Option<Scout>,
    /// Live frames the start conditions have held, running.
    ready: u32,
    /// Smoothed cost of one scout frame, microseconds.
    frame_us: u64,
    /// Consecutive ticks that ran no scout frame.
    starved: u32,
}

/// The live machine and its surroundings, for one [`ScoutDriver::tick`].
pub struct LiveView<'a> {
    pub stepper: &'a EmuStepper,
    pub rom: &'a [u8],
    pub canvases: &'a mut LayerCanvasAccumulator,
    pub scene: SceneId,
    /// The live frame's `(width, height)`.
    pub size: (u16, u16),
    pub scrolls: ScrollFn<'a>,
}

impl ScoutDriver {
    /// Arm (or, with `None`, disarm) scouting. Forgets the scene, so the
    /// current one is scouted afresh.
    pub fn set(&mut self, setup: Option<ScoutSetup>) {
        self.setup = setup;
        self.scene = None;
        self.scout = None;
        self.done = false;
        self.ready = 0;
    }

    /// Whether a scout is running now.
    #[must_use]
    pub fn running(&self) -> bool {
        self.scout.is_some()
    }

    /// One live frame has just run. `spare` is the time the loop can lend
    /// (`None` when it is not paced).
    pub fn tick(&mut self, live: LiveView<'_>, spare: Option<std::time::Duration>) {
        let Some(setup) = self.setup.clone() else {
            return;
        };
        if self.scene != Some(live.scene.0) {
            // A new scene: whatever ran belongs to the old one.
            self.scene = Some(live.scene.0);
            self.scout = None;
            self.done = false;
            self.attempts = 0;
            self.ready = 0;
            self.settle_left = setup.plan.settle_frames;
        }
        if self.done {
            return;
        }
        if self.scout.is_none() {
            if self.settle_left > 0 {
                self.settle_left -= 1;
                return;
            }
            if !self.start(&setup, &live) {
                return;
            }
        }
        // Paced: only the time the loop would sleep anyway (less a margin).
        // Not paced (fast-forward, tests): a small fixed slice.
        let budget = if setup.turbo {
            std::time::Duration::from_secs(1)
        } else {
            spare.map_or(UNPACED_SLICE, |s| s.saturating_sub(PACED_MARGIN))
        };
        self.run_slice(&setup, live, budget, spare.is_none());
    }

    /// Try to start the scene's scout; `false` = not now.
    fn start(&mut self, setup: &ScoutSetup, live: &LiveView<'_>) -> bool {
        let retry = |me: &mut Self| {
            me.attempts += 1;
            me.settle_left = RETRY_FRAMES;
            if me.attempts >= MAX_ATTEMPTS {
                me.done = true;
            }
        };
        // Not ready (the player is dead, in a cutscene): wait, without
        // using up a retry; the live game will get there or it will not.
        if setup
            .plan
            .start_when
            .iter()
            .any(|&(addr, v)| live.stepper.peek_bus(addr) != v)
        {
            self.ready = 0;
            return false;
        }
        self.ready += 1;
        if self.ready < READY_FRAMES {
            return false;
        }
        if !camera_matches(&setup.plan, live.stepper, live.scrolls) {
            retry(self);
            return false;
        }
        let Ok(snapshot) = live.stepper.snapshot() else {
            retry(self);
            return false;
        };
        match Scout::start(
            live.rom,
            snapshot,
            setup.plan.clone(),
            live.canvases_ref(),
            live.scene,
            live.size,
        ) {
            Ok(scout) => {
                ScoutTelemetry::add(&setup.telemetry.started, 1);
                self.scout = Some(scout);
                true
            }
            Err(_) => {
                retry(self);
                false
            }
        }
    }

    fn run_slice(
        &mut self,
        setup: &ScoutSetup,
        live: LiveView<'_>,
        budget: std::time::Duration,
        unpaced: bool,
    ) {
        let Some(scout) = self.scout.as_mut() else {
            return;
        };
        let t0 = std::time::Instant::now();
        let mut ran = 0u32;
        // A frame runs only if the cost of the last ones says it fits the
        // budget, so a slice never makes a live frame late. A loop that is
        // already behind (no spare) would starve the scout for good, so
        // every STARVE_TICKS-th tick it gets one frame anyway; unpaced
        // loops (nothing to be late for) always get one.
        let forced_first = unpaced || self.starved >= STARVE_TICKS;
        let mut forced = false;
        // Bounded: at most MAX_FRAMES_PER_TICK iterations, each of which
        // either runs a frame or leaves the loop.
        while ran < MAX_FRAMES_PER_TICK && scout.status() == ScoutStatus::Running {
            let est = std::time::Duration::from_micros(self.frame_us);
            let fits = t0.elapsed() + est <= budget;
            if !(fits || ran == 0 && forced_first) {
                break;
            }
            forced |= !fits;
            let f0 = std::time::Instant::now();
            scout.step(live.scrolls);
            let us = f0.elapsed().as_micros() as u64;
            self.frame_us = (self.frame_us * 3 + us) / 4;
            ran += 1;
        }
        self.starved = if ran == 0 { self.starved + 1 } else { 0 };
        let finished = matches!(scout.status(), ScoutStatus::Finished(_));
        let mut merged = 0;
        if finished || scout.frames_since_merge() >= MERGE_EVERY {
            merged = scout.merge_into(live.canvases);
        }
        let us = t0.elapsed().as_micros() as u64;
        let t = &setup.telemetry;
        ScoutTelemetry::add(&t.frames, u64::from(ran));
        ScoutTelemetry::add(&t.cells, merged as u64);
        ScoutTelemetry::add(&t.wall_us, us);
        ScoutTelemetry::add(&t.slices, u64::from(ran > 0));
        ScoutTelemetry::add(&t.forced, u64::from(forced));
        if ran > 0 && !forced {
            ScoutTelemetry::add(&t.over_us, us.saturating_sub(budget.as_micros() as u64));
        }
        t.max_slice_us
            .fetch_max(us, std::sync::atomic::Ordering::Relaxed);
        if finished {
            let useful = scout.frames_run() >= MIN_USEFUL_FRAMES;
            if useful {
                if let Ok(mut ends) = t.last_ends.lock() {
                    ends.clone_from(&scout.ends);
                }
                ScoutTelemetry::add(&t.finished, 1);
            } else {
                // It began in the middle of a transition: try again later.
                ScoutTelemetry::add(&t.aborted, 1);
                self.attempts += 1;
                self.settle_left = RETRY_FRAMES;
                self.ready = 0;
            }
            self.scout = None;
            self.done = useful || self.attempts >= MAX_ATTEMPTS;
        }
    }
}

impl LiveView<'_> {
    fn canvases_ref(&self) -> &LayerCanvasAccumulator {
        self.canvases
    }
}

/// Whether the game's camera word agrees with the first stitched layer's
/// live scroll (the game is in a level and the recipe's address is right).
fn camera_matches(plan: &ScoutPlan, stepper: &EmuStepper, scrolls: ScrollFn<'_>) -> bool {
    let Some(&addr) = plan.camera.first() else {
        return true;
    };
    let Some(layer) = scrolls(stepper).and_then(|s| s.into_iter().flatten().next()) else {
        return false;
    };
    let wrap = i64::from(layer.tilemap_width.max(1));
    let cam = i64::from(word(stepper, addr)).rem_euclid(wrap);
    let d = (cam - i64::from(layer.hofs)).rem_euclid(wrap);
    d.min(wrap - d) <= CAMERA_TOLERANCE_PX
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(strategy: ScoutStrategy) -> ScoutPlan {
        ScoutPlan {
            strategy,
            camera: vec![0x7E_001A, 0x7E_1462],
            player_x: vec![0x7E_0094],
            step_px: 4,
            frame_budget: 10,
            stall_frames: 3,
            settle_frames: 0,
            dirs: vec![ScoutDir::Right],
            hold: vec![(0x7E_0100, 7)],
            start_when: Vec::new(),
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

    #[test]
    fn player_move_pokes_the_player_word_not_the_camera() {
        let p = plan(ScoutStrategy::PlayerMove).pokes(ScoutDir::Right, 0x0100, 0x0300);
        assert_eq!(p[..2], [(0x7E_0094, 0x04), (0x7E_0095, 0x03)]);
        assert!(p.iter().all(|&(a, _)| a != 0x7E_001A));
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
            last = r.observe(&pl, (i * 4) as u16, 100 + i as usize);
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
    const NES_CAMERA_X: u32 = 0x6029 + 2; // camera_x, FORMAT.md "Documented RAM addresses"
    const NES_PLAYER_X: u32 = 0x6029;

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

    /// The NES has no layer registers to read here: the fixture's own camera
    /// variable stands in for the scroll.
    fn nes_scrolls(st: &EmuStepper) -> Option<[Option<LayerScroll>; LAYERS]> {
        let cam = word(st, NES_CAMERA_X);
        Some([
            Some(LayerScroll {
                hofs: cam % 512,
                vofs: 0,
                tilemap_width: 512,
                tilemap_height: 240,
            }),
            None,
            None,
            None,
        ])
    }

    fn width_of(acc: &LayerCanvasAccumulator, scene: u64) -> usize {
        acc.scene_canvases(scene).map_or(0, |s| s[0].width())
    }

    #[test]
    fn a_scout_on_rf_scroller_extends_the_canvas_beyond_what_the_live_run_visited() {
        let Some(rom) = fixture() else { return };
        let mut live_st = EmuStepper::open(&rom).expect("fixture opens");
        let mut sink = ScoutSink {
            bundle: FrameBundleBuilder::new(256, 240),
        };
        let scene = SceneId(1);
        let mut live = LayerCanvasAccumulator::new();
        let mut prev = nes_scrolls(&live_st);
        // Boot, then hold Right for a short walk: the camera has only moved a
        // little, so most of the level is unvisited.
        for f in 0..200u32 {
            let mut input = InputFrame::empty();
            if f >= 60 {
                input.ports[0] = RIGHT;
            }
            live_st.latch_and_advance_frame(input, &mut sink);
            let bundle = sink.bundle.take(live_st.frame_count());
            let now = nes_scrolls(&live_st);
            let drawn_with = std::mem::replace(&mut prev, now);
            live.observe_frame(&bundle, scene, drawn_with.as_ref());
        }
        let live_width = width_of(&live, scene.0);
        let cam_now = word(&live_st, NES_CAMERA_X);
        assert!(live_width >= 256, "the live run stitched something");
        let before = live.scene_canvases(scene.0).unwrap()[0].clone();

        let plan = ScoutPlan {
            strategy: ScoutStrategy::PlayerMove,
            camera: vec![NES_CAMERA_X],
            player_x: vec![NES_PLAYER_X],
            step_px: 4,
            frame_budget: 400,
            stall_frames: 60,
            settle_frames: 0,
            dirs: vec![ScoutDir::Right],
            hold: Vec::new(),
            start_when: Vec::new(),
        };
        let snapshot = live_st.snapshot().expect("snapshot");
        let mut scout =
            Scout::start(&rom, snapshot, plan, &live, scene, (256, 240)).expect("starts");
        // Law 8: a bounded loop; the plan's own budget ends it well before.
        for _ in 0..2_000 {
            if scout.step(&nes_scrolls) != ScoutStatus::Running {
                break;
            }
        }
        assert!(
            matches!(scout.status(), ScoutStatus::Finished(_)),
            "the budget/stall rules ended the pass: {:?}",
            scout.ends
        );
        let merged = scout.merge_into(&mut live);
        assert!(merged > 0, "the scout found unknown cells");
        let after = &live.scene_canvases(scene.0).unwrap()[0];
        assert!(
            after.width() > live_width,
            "canvas {} -> {} wide (camera was at {cam_now})",
            live_width,
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
        // And the live machine was only ever read.
        assert_eq!(word(&live_st, NES_CAMERA_X), cam_now);
    }

    #[test]
    fn a_driver_waits_out_the_settle_frames_then_scouts_the_scene_once() {
        let Some(rom) = fixture() else { return };
        let mut st = EmuStepper::open(&rom).expect("opens");
        let mut sink = ScoutSink {
            bundle: FrameBundleBuilder::new(256, 240),
        };
        let scene = SceneId(7);
        let mut live = LayerCanvasAccumulator::new();
        let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
        let mut driver = ScoutDriver::default();
        driver.set(Some(ScoutSetup {
            plan: ScoutPlan {
                strategy: ScoutStrategy::PlayerMove,
                camera: vec![NES_CAMERA_X],
                player_x: vec![NES_PLAYER_X],
                step_px: 4,
                frame_budget: 300,
                stall_frames: 40,
                settle_frames: 90,
                dirs: vec![ScoutDir::Right],
                hold: Vec::new(),
                start_when: Vec::new(),
            },
            telemetry: telemetry.clone(),
            turbo: false,
        }));
        let mut prev = nes_scrolls(&st);
        let mut first_run_tick = None;
        // Bounded: 600 live frames.
        for f in 0..600u32 {
            let mut input = InputFrame::empty();
            input.ports[0] = if f >= 60 { RIGHT } else { 0 };
            st.latch_and_advance_frame(input, &mut sink);
            let bundle = sink.bundle.take(st.frame_count());
            let now = nes_scrolls(&st);
            let drawn_with = std::mem::replace(&mut prev, now);
            live.observe_frame(&bundle, scene, drawn_with.as_ref());
            driver.tick(
                LiveView {
                    stepper: &st,
                    rom: &rom,
                    canvases: &mut live,
                    scene,
                    size: (256, 240),
                    scrolls: &nes_scrolls,
                },
                None,
            );
            if first_run_tick.is_none() && driver.running() {
                first_run_tick = Some(f);
            }
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
        let mut sink = ScoutSink {
            bundle: FrameBundleBuilder::new(256, 240),
        };
        let scene = SceneId(1);
        let mut live = LayerCanvasAccumulator::new();
        let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
        let mut driver = ScoutDriver::default();
        // A byte that never holds this value: the game is never "ready".
        let never = (NES_PLAYER_X, 0xEE);
        driver.set(Some(ScoutSetup {
            plan: ScoutPlan {
                strategy: ScoutStrategy::PlayerMove,
                camera: vec![NES_CAMERA_X],
                player_x: vec![NES_PLAYER_X],
                step_px: 4,
                frame_budget: 100,
                stall_frames: 20,
                settle_frames: 0,
                dirs: vec![ScoutDir::Right],
                hold: Vec::new(),
                start_when: vec![never],
            },
            telemetry: telemetry.clone(),
            turbo: false,
        }));
        let mut prev = nes_scrolls(&st);
        // Bounded: 150 live frames.
        for _ in 0..150u32 {
            st.latch_and_advance_frame(InputFrame::empty(), &mut sink);
            let bundle = sink.bundle.take(st.frame_count());
            let now = nes_scrolls(&st);
            let drawn_with = std::mem::replace(&mut prev, now);
            live.observe_frame(&bundle, scene, drawn_with.as_ref());
            driver.tick(
                LiveView {
                    stepper: &st,
                    rom: &rom,
                    canvases: &mut live,
                    scene,
                    size: (256, 240),
                    scrolls: &nes_scrolls,
                },
                None,
            );
        }
        assert_eq!(ScoutTelemetry::get(&telemetry.started), 0);
        assert!(!driver.running());
    }
}
