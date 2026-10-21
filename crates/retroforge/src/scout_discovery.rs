//! Ticket W33-07: from what the finder SAW to a `[scout]` table that WORKS.
//!
//! [`rf_enhance::scout_finder::ScoutFinder`] names candidates by watching play
//! (camera, a few player-X words, a few Y bytes, a few mode bytes). It cannot
//! say which of them the game obeys: a copy of the player's X that the game
//! recopies each frame, a sprite-table copy of his Y, a counter that happened
//! to sit still while the player stood. Those questions need a machine to try
//! things on, which is what the scout already is. This module runs short
//! trial scouts from one snapshot of the live game (one second machine, which
//! [`Scout::restart_with`] puts back to the snapshot between trials) and keeps
//! what works:
//!
//! 1. **Recipe.** The first player-X candidate under `player_move`, then `walk`
//!    on a console placed by bands, then the other player words (mostly copies
//!    the game recopies, so tried last), each with no hold and with each Y
//!    candidate held
//!    [`FIRST_DELTA`] pixels "above" where the player stood, for
//!    [`TRIAL_FRAMES`] frames: the first mover whose best recipe grows the
//!    canvases to two screens (the camera followed it, the player was not cut
//!    short) wins. A hold has to beat the same mover without one by a tenth.
//! 2. **Height.** The winning hold is tuned over [`DELTAS`].
//!
//! Everything it keeps is written as observation rows
//! (`rf_enhance::scout_finder::SOURCE`). The census-style acceptance lives in
//! `tests/w33_07_discovery.rs`.

use rf_enhance::canvas::CanvasModel;
use rf_enhance::scene_identity::SceneId;
use rf_enhance::scout_finder::{
    GateCandidate, Observed, PlayerCandidate, Row, Skeleton, YCandidate,
};

use crate::scout::{Scout, ScoutDir, ScoutEnd, ScoutPlan, ScoutStatus, ScoutStrategy};
use crate::stepper::MachineSnapshot;

/// Frames of a trial (long enough for the first enemy of a level to arrive).
pub const TRIAL_FRAMES: u32 = 300;
/// Pixels above the standing Y the first hold is tried at.
pub const FIRST_DELTA: i32 = 120;
/// The heights the best candidate is tuned over (pixels above standing).
pub const DELTAS: [i32; 6] = [200, 160, 120, 88, 64, 40];
/// Frames of the height trials: long enough to cover a whole level, because a
/// hold too high or too low only shows where the level has its ceilings and
/// its pipes.
pub const TUNE_FRAMES: u32 = 1100;
/// Pixels per frame the trials scout at.
pub const TRIAL_STEP_PX: u16 = 4;
/// W35-07: a `walk` trial's patience. A player who walks at a pixel a frame takes a screen's width
/// (256 frames) to reach the far edge of the screen the trial starts on, and a game that moves
/// its camera only when the player gets there (the Legend of Zelda's screen flips) has stitched
/// nothing new for all of it; the other recipes' 90 ended its walk 30 frames short of the edge.
pub const WALK_STALL_FRAMES: u32 = 260;
/// W35-08: frames a walk goes right before the Left trial is tried from where it stands.
pub const MIDWAY_FRAMES: u32 = 480;
/// Y candidates tried.
pub const MAX_Y: usize = 16;
/// A trial that grew the canvases to less than this many tenths of a screen
/// did not follow the player anywhere. (W35-03: 2.0 screens became 1.4: a game
/// whose player walks one pixel a frame, Castlevania, shows 1.5 screens after
/// the 300 trial frames, with the first 80 of them spent before the camera
/// starts; a trial that did not follow shows less than one.)
const MIN_SCREEN_TENTHS: usize = 14;

/// A vertical trial that grew the canvases to this many fifths of a screen
/// (counting the first screen as five) moved the screen: the Y word is the
/// game's.
const VERTICAL_FIFTHS: usize = 6;
/// Frames of a vertical trial (4 px a frame covers a three-screen shaft).
pub const VERTICAL_FRAMES: u32 = 420;

/// What the trials run on.
pub struct Context<'a> {
    pub rom: &'a [u8],
    /// The live model at the snapshot (axes and trackers are copied, no
    /// canvases).
    pub live: &'a CanvasModel,
    pub scene: SceneId,
    pub size: (u16, u16),
    pub snes: bool,
    pub scrolls: crate::scout::ScrollReader,
    /// The profile's `[scene]` key, if it has one (the live model is fed it).
    pub scene_key: Option<crate::scene_key::SceneKeySpec>,
    /// A machine in a room that scrolls vertically, if the session met one
    /// (the Y recipe cannot be tried anywhere else).
    pub vertical: Option<Vertical<'a>>,
}

/// Ticket W35-02: where the player's Y word is tried. A Y byte is named by
/// what it does at a jump, which says nothing about whether the game obeys a
/// poke of it (a sprite table holds a copy, a speed is not a position); the
/// only judge is a scout that moves it in a room that scrolls vertically.
pub struct Vertical<'a> {
    /// A snapshot of the live machine standing in that room.
    pub snapshot: MachineSnapshot,
    /// The live model then (axes and trackers are copied, no canvases): it
    /// has to know the scene scrolls vertically.
    pub live: &'a CanvasModel,
    pub scene: SceneId,
}

/// How one trial ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// Cells inside the canvases' bounding boxes.
    pub extent: usize,
    /// The pass ended by a scene change (a death, a respawn): not alive.
    pub died: bool,
    pub frames: u64,
}

impl Outcome {
    /// Ordering key: alive first, then how much was seen.
    fn score(self) -> (bool, usize) {
        (!self.died, self.extent)
    }
}

/// What the discovery found out and how it got there (for logs and tests).
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// One line per trial.
    pub trials: Vec<String>,
}

/// Why no table came out.
#[derive(Debug)]
pub enum DiscoveryError {
    /// The finder named no camera (the scout reads one on the SNES).
    NoCamera,
    /// No strategy made the canvases grow: the game does not follow what the
    /// scout does.
    NothingFollows(Report),
    /// The second machine could not be opened or restored.
    Machine(String),
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiscoveryError::NoCamera => write!(f, "no camera was found"),
            DiscoveryError::NothingFollows(r) => {
                write!(f, "the game's camera did not follow any recipe tried")?;
                for t in r.trials.iter().take(30) {
                    write!(f, "\n  {t}")?;
                }
                Ok(())
            }
            DiscoveryError::Machine(why) => write!(f, "the trial machine failed: {why}"),
        }
    }
}

impl std::error::Error for DiscoveryError {}

/// Bus address of a finder offset.
#[must_use]
pub fn bus_address(snes: bool, offset: usize) -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    let o = offset as u32;
    if snes {
        0x7E_0000 + o
    } else if o < 0x0800 {
        o
    } else {
        0x6000 + (o - 0x0800)
    }
}

/// The plan every trial starts from.
fn base_plan(ctx: &Context<'_>, obs: &Observed) -> ScoutPlan {
    let camera = obs
        .camera
        .map(|c| vec![bus_address(ctx.snes, c.x.offset)])
        .unwrap_or_default();
    ScoutPlan {
        strategy: ScoutStrategy::PlayerMove,
        camera: if ctx.snes { camera } else { Vec::new() },
        player_x: Vec::new(),
        player_x_hi: Vec::new(),
        camera_y: Vec::new(),
        player_y: Vec::new(),
        player_y_hi: Vec::new(),
        step_px: TRIAL_STEP_PX,
        frame_budget: TRIAL_FRAMES,
        stall_frames: 90,
        settle_frames: 0,
        dirs: vec![ScoutDir::Right],
        hold: Vec::new(),
        start_when: Vec::new(),
        pin: true,
        scene_key: ctx.scene_key.clone(),
    }
}

fn with_player(mut plan: ScoutPlan, snes: bool, p: &PlayerCandidate) -> ScoutPlan {
    plan.strategy = ScoutStrategy::PlayerMove;
    plan.player_x = vec![bus_address(snes, p.lo)];
    plan.player_x_hi = p.hi.map(|h| vec![bus_address(snes, h)]).unwrap_or_default();
    plan
}

fn run(scout: &mut Scout, plan: ScoutPlan, ctx: &Context<'_>) -> Result<Outcome, DiscoveryError> {
    let frames = plan.frame_budget;
    let (plan_strategy, plan_dirs, plan_step, plan_hold) = (
        plan.strategy,
        plan.dirs.clone(),
        plan.step_px,
        plan.hold.clone(),
    );
    scout
        .restart_with(plan)
        .map_err(|e| DiscoveryError::Machine(format!("{e:?}")))?;
    let reader = ctx.scrolls;
    let mut died = false;
    // Bounded: the trial's own frame budget.
    for _ in 0..frames {
        if let ScoutStatus::Finished(end) = scout.step(&reader) {
            died = end == ScoutEnd::SceneChanged;
            break;
        }
    }
    let out = Outcome {
        extent: scout.main_extent(),
        died,
        frames: scout.frames_run(),
    };
    if std::env::var_os("RF_TRACE_TRIALS").is_some() {
        eprintln!(
            "TRIAL {:?} dirs {:?} step {} hold {:?}: extent {} width {} died {} frames {}",
            plan_strategy,
            plan_dirs,
            plan_step,
            plan_hold,
            out.extent,
            scout.width(),
            out.died,
            out.frames
        );
    }
    Ok(out)
}

/// The hold rows for a Y candidate held `delta` pixels above where the player
/// stood: the low byte, and on the SNES (which keeps Y in words) the high byte
/// too when `wide`.
#[allow(clippy::cast_possible_truncation)]
fn hold_pokes(scout: &Scout, snes: bool, wide: bool, y: &YCandidate, delta: i32) -> Vec<(u32, u8)> {
    let addr = bus_address(snes, y.offset);
    let up = i32::from(y.up);
    if snes && wide {
        let base = i32::from(scout.peek_word(addr));
        let v = (base + up * delta).clamp(0, 0xFFFF);
        vec![(addr, v as u8), (addr + 1, (v >> 8) as u8)]
    } else {
        let base = i32::from(scout.peek_word(addr) & 0xFF);
        let v = (base + up * delta).clamp(0, 0xFF);
        vec![(addr, v as u8)]
    }
}

/// What a hold was tried as: the bytes, and how far above standing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hold {
    pokes: Vec<(u32, u8)>,
    delta: i32,
    offset: usize,
    standing: u8,
}

/// The finder's confidence a gate needs to be written as a `start_when` row
/// (W35-03: its confidence is the steady share times what it does at the cuts,
/// not the old constancy times difference, so 0.9 became 0.7).
const MIN_GATE_CONFIDENCE: f32 = 0.7;

/// A gate that is false for a stretch of ordinary play keeps the scout from
/// ever starting. The share of a scouting run's frames a gate's value must hold
/// on (the run moves the player through every pose the level allows).
const GATE_SURVIVAL: f32 = 0.98;

/// W35-06: the most a camera word moves in a frame the scout is walking it (the walking pace the
/// finder uses, `scout_finder`'s `|step| <= 4`). A bigger step is a scripted scroll or a rewind:
/// the game, not the scout, moves the picture.
const CONTROL_STEP: i32 = 4;
/// Frames after such a step before the scout is in control again (a fade, a load).
const CONTROL_SETTLE: usize = 30;
/// Frames of the scout's pokes that move no camera (either axis) before the game is judged to hold
/// the player (a speech box, a lock): the finder's own `speech` test.
const CONTROL_FROZEN: usize = 24;
/// Fewer judged frames than this say nothing: the gate is judged on every frame instead.
const MIN_JUDGED: usize = 30;

/// One frame of a scouting run: the scout's camera words (x, y) and which gates held. (Not the
/// player's: the scout pokes those every frame, a held player included.)
struct Sample {
    pose: [u16; 2],
    held: Vec<bool>,
}

/// Per gate, the share of the run's frames the gate held on, counting only the frames the SCOUT
/// was in control of (W35-06). Left out: a frame within [`CONTROL_SETTLE`] of a camera step above
/// [`CONTROL_STEP`] (the game's own scroll between areas, a cut, the rewind between passes), and
/// every frame of a run of at least [`CONTROL_FROZEN`] frames where the camera did not move
/// though the scout poked the player on (the game holds the player: A Link to the Past's speech boxes come every ~4,500
/// frames and read mode `$0E` where the real mode byte reads 9; a gate that is false there is
/// RIGHT to be, and must not be dropped for it). A run with fewer than [`MIN_JUDGED`] such frames
/// is judged on all of them.
fn control_survival(run: &[Sample], gates: usize) -> Vec<f32> {
    let mut judged = vec![true; run.len()];
    let mut settle = 0usize;
    for (i, w) in run.windows(2).enumerate() {
        let step = |axis: usize| i32::from(w[1].pose[axis].wrapping_sub(w[0].pose[axis]) as i16);
        if step(0).abs() > CONTROL_STEP || step(1).abs() > CONTROL_STEP {
            settle = CONTROL_SETTLE;
        }
        if settle > 0 {
            judged[i + 1] = false;
            settle -= 1;
        }
    }
    // Runs of frames where nothing moved, marked whole (the first frames of a speech read the box too).
    let mut from = 0;
    for i in 1..=run.len() {
        if i == run.len() || run[i].pose != run[from].pose {
            if i - from >= CONTROL_FROZEN {
                judged[from..i].fill(false);
            }
            from = i;
        }
    }
    let all = judged.iter().all(|j| !j) || judged.iter().filter(|j| **j).count() < MIN_JUDGED;
    let keep = |i: usize| all || judged[i];
    let total = (0..run.len()).filter(|&i| keep(i)).count();
    (0..gates)
        .map(|g| {
            let held = (0..run.len())
                .filter(|&i| keep(i) && run[i].held[g])
                .count();
            #[allow(clippy::cast_precision_loss)]
            {
                held as f32 / total.max(1) as f32
            }
        })
        .collect()
}

/// Run `plan` and count, per gate candidate, the frames its value held while the scout was in
/// control ([`control_survival`]).
fn gate_survival(
    scout: &mut Scout,
    plan: ScoutPlan,
    gates: &[GateCandidate],
    ctx: &Context<'_>,
) -> Result<Vec<f32>, DiscoveryError> {
    let frames = plan.frame_budget;
    scout
        .restart_with(plan)
        .map_err(|e| DiscoveryError::Machine(format!("{e:?}")))?;
    let reader = ctx.scrolls;
    let mut run = Vec::new();
    // Bounded: the plan's own frame budget.
    for _ in 0..frames {
        let status = scout.step(&reader);
        run.push(Sample {
            pose: scout.camera_pose(),
            held: gates
                .iter()
                .map(|g| {
                    scout.peek_word(bus_address(ctx.snes, g.offset)) & 0xFF == u16::from(g.value)
                })
                .collect(),
        });
        if matches!(status, ScoutStatus::Finished(_)) {
            break;
        }
    }
    Ok(control_survival(&run, gates.len()))
}

/// The player's Y word, by a vertical pass from a room that scrolls that way:
/// each Y candidate is poked as the player's Y (the whole word, then the byte
/// alone) and the one that scrolls the screen furthest wins.
fn confirm_y(
    ctx: &Context<'_>,
    vertical: Option<&Vertical<'_>>,
    obs: &Observed,
    mover: &ScoutPlan,
    report: &mut Report,
) -> Result<VerticalOutcome, DiscoveryError> {
    let (Some(v), true) = (vertical, ctx.snes) else {
        return Ok(VerticalOutcome::default());
    };
    let Some(cam_y) = obs.camera.and_then(|c| c.y) else {
        return Ok(VerticalOutcome::default());
    };
    let mut base = mover.clone();
    base.hold.clear();
    base.camera_y = vec![bus_address(true, cam_y.offset)];
    base.dirs = vec![ScoutDir::Down, ScoutDir::Up];
    base.frame_budget = VERTICAL_FRAMES;
    base.stall_frames = 120;
    let snapshot = v.snapshot.clone();
    let mut scout = Scout::start(ctx.rom, snapshot, base.clone(), v.live, v.scene, ctx.size)
        .map_err(|e| DiscoveryError::Machine(format!("{e:?}")))?;
    let floor = usize::from(ctx.size.0) * usize::from(ctx.size.1) * VERTICAL_FIFTHS / 5;
    let mut best: Option<(YCandidate, bool, usize)> = None;
    for y in obs.ys.iter().take(MAX_Y) {
        for wide in [true, false] {
            let addr = bus_address(true, y.offset);
            let mut plan = base.clone();
            plan.player_y = vec![addr];
            plan.player_y_hi = if wide { vec![addr + 1] } else { Vec::new() };
            let out = run(&mut scout, plan, ctx)?;
            report.trials.push(format!(
                "vertical {:#x} wide {wide}: extent {} died {} after {} frames",
                y.offset, out.extent, out.died, out.frames
            ));
            if !out.died && out.extent >= floor && best.as_ref().is_none_or(|b| out.extent > b.2) {
                best = Some((*y, wide, out.extent));
            }
            // A wide word that moved the screen makes the byte alone moot.
            if wide && out.extent >= floor {
                break;
            }
        }
    }
    // A mode byte holds its value in this room too: sampled while the scout
    // that moves the Y word runs (else at the snapshot).
    let gates_hold = match &best {
        Some((y, wide, _)) => {
            let addr = bus_address(true, y.offset);
            let mut plan = base.clone();
            plan.player_y = vec![addr];
            plan.player_y_hi = if *wide { vec![addr + 1] } else { Vec::new() };
            gate_survival(&mut scout, plan, &obs.gates, ctx)?
                .into_iter()
                .map(|f| f >= GATE_SURVIVAL)
                .collect()
        }
        None => {
            scout
                .restart_with(base.clone())
                .map_err(|e| DiscoveryError::Machine(format!("{e:?}")))?;
            obs.gates
                .iter()
                .map(|g| scout.peek_word(bus_address(true, g.offset)) & 0xFF == u16::from(g.value))
                .collect()
        }
    };
    Ok(VerticalOutcome {
        y_word: best,
        gates_hold: Some(gates_hold),
    })
}

/// What the vertical room told.
#[derive(Default)]
struct VerticalOutcome {
    /// The Y word the game obeys, whether it is the whole word, and how many
    /// cells the canvases reached.
    y_word: Option<(YCandidate, bool, usize)>,
    /// Per finder gate candidate, whether its value holds in that room.
    gates_hold: Option<Vec<bool>>,
}

/// Run the trials and build the table.
///
/// # Errors
/// [`DiscoveryError`].
pub fn discover(
    snapshot: MachineSnapshot,
    ctx: Context<'_>,
    obs: &Observed,
) -> Result<(Skeleton, Report), DiscoveryError> {
    if ctx.snes && obs.camera.is_none() {
        return Err(DiscoveryError::NoCamera);
    }
    let mut report = Report::default();
    report.trials.push(format!(
        "candidates: players {:?}, ys {:?}",
        obs.players.iter().map(|p| p.lo).collect::<Vec<_>>(),
        obs.ys.iter().map(|y| y.offset).collect::<Vec<_>>()
    ));
    let base = base_plan(&ctx, obs);
    let seed = ctx.live;
    // W35-04: a top-down game scrolls both ways wherever the player stands, so the vertical room
    // is the room the trials start in.
    let main_room = (obs.top_down && ctx.vertical.is_none()).then(|| Vertical {
        snapshot: snapshot.clone(),
        live: ctx.live,
        scene: ctx.scene,
    });
    let mut scout = Scout::start(ctx.rom, snapshot, base.clone(), seed, ctx.scene, ctx.size)
        .map_err(|e| DiscoveryError::Machine(format!("{e:?}")))?;
    let bands = scout.places_by_bands();
    let (w, h) = ctx.size;
    let floor = MIN_SCREEN_TENTHS * usize::from(w) * usize::from(h) / 10;

    // Every recipe: a way to move (each player word the finder named, and
    // `walk` where the console is placed by bands) with no hold, and with each
    // Y candidate held. The game may only follow a player who floats (Super
    // Mario Bros. 3 does not scroll for one standing on the ground), so the
    // strategy and the hold are chosen together.
    // The player words after the first are mostly copies of it (a game recopies
    // its player's X into other variables), so they are tried only when the
    // first one, and `walk`, got nowhere.
    let mut movers: Vec<(Option<PlayerCandidate>, ScoutPlan)> = Vec::new();
    if let Some(p) = obs.players.first() {
        movers.push((Some(*p), with_player(base.clone(), ctx.snes, p)));
    }
    if bands {
        let mut walk = base.clone();
        walk.strategy = ScoutStrategy::Walk;
        walk.stall_frames = WALK_STALL_FRAMES;
        movers.push((None, walk));
    }
    for p in obs.players.iter().skip(1).take(2) {
        movers.push((Some(*p), with_player(base.clone(), ctx.snes, p)));
    }
    type Winner = (Option<PlayerCandidate>, ScoutPlan, Option<Hold>, Outcome);
    let mut best: Option<Winner> = None;
    for (player, mut plan) in movers {
        plan.frame_budget = TRIAL_FRAMES;
        let mut none = run(&mut scout, plan.clone(), &ctx)?;
        report.trials.push(format!(
            "{:?} {:?} no hold: extent {} died {} after {} frames",
            plan.strategy,
            player.map(|p| p.lo),
            none.extent,
            none.died,
            none.frames
        ));
        // W35-04: a player standing at the right end of the world (a camera clamped at the edge
        // of an overworld screen) scrolls nowhere to the right: try the other way before judging
        // the recipe.
        if none.extent < floor && plan.strategy == ScoutStrategy::PlayerMove {
            let mut left = plan.clone();
            left.dirs = vec![ScoutDir::Left];
            let l = run(&mut scout, left.clone(), &ctx)?;
            report.trials.push(format!(
                "{:?} {:?} no hold, Left: extent {} died {} after {} frames",
                plan.strategy,
                player.map(|p| p.lo),
                l.extent,
                l.died,
                l.frames
            ));
            if l.score() > none.score() {
                (none, plan) = (l, left);
            }
        }
        let mut tried: Vec<(Option<Hold>, Outcome)> = vec![(None, none)];
        // W35-04: a top-down game has no ground to float above: a Y poke is a move.
        let hold_ys: &[YCandidate] = if obs.top_down { &[] } else { &obs.ys };
        for y in hold_ys.iter().take(MAX_Y) {
            for wide in [true, false] {
                if !ctx.snes && wide {
                    continue;
                }
                // The machine is at the snapshot after a restart: peek there.
                scout
                    .restart_with(plan.clone())
                    .map_err(|e| DiscoveryError::Machine(format!("{e:?}")))?;
                let pokes = hold_pokes(&scout, ctx.snes, wide, y, FIRST_DELTA);
                let mut trial = plan.clone();
                trial.hold = pokes.clone();
                let out = run(&mut scout, trial, &ctx)?;
                report.trials.push(format!(
                    "{:?} {:?} hold {:#x} wide {wide} {:?}: extent {} died {} after {} frames",
                    plan.strategy,
                    player.map(|p| p.lo),
                    y.offset,
                    pokes,
                    out.extent,
                    out.died,
                    out.frames
                ));
                let hold = Hold {
                    pokes,
                    delta: FIRST_DELTA,
                    offset: y.offset,
                    standing: y.standing,
                };
                let beat = out.score() > none.score();
                let inert = out.extent == none.extent && out.died == none.died;
                tried.push((Some(hold), out));
                // A wide hold that works makes the narrow one beside it moot,
                // and so does one that changed nothing at all (the bytes are
                // not the game's).
                if beat || inert {
                    break;
                }
            }
        }
        // `player_move` is listed first and wins ties; a hold has to beat
        // what the same mover did without one by a tenth to be kept.
        let (mut hold, mut out) = (None, none);
        for (h, o) in tried.into_iter().skip(1) {
            if o.score() > out.score() && o.extent > out.extent + out.extent / 10 {
                (hold, out) = (h, o);
            }
        }
        if out.extent >= floor {
            // W35-03: a recipe that scrolls the screen when the scout does NOT
            // move the player is not following anything: a hold that makes the
            // game walk Simon by itself (Castlevania, Y poked to the top of
            // the screen) passed every trial and never went left. The control
            // is the same recipe with a step of 0.
            if plan.strategy == ScoutStrategy::PlayerMove {
                let mut control = plan.clone();
                control.step_px = 0;
                control.hold = hold.as_ref().map(|h| h.pokes.clone()).unwrap_or_default();
                let c = run(&mut scout, control, &ctx)?;
                report.trials.push(format!(
                    "{:?} {:?} control (step 0, same hold): extent {} width {} died {} after {} frames",
                    plan.strategy,
                    player.map(|p| p.lo),
                    c.extent,
                    scout.width(),
                    c.died,
                    c.frames
                ));
                // Sideways only: a hold may move a vertical camera by itself.
                if scout.width() * 10 >= usize::from(w) * MIN_SCREEN_TENTHS {
                    report
                        .trials
                        .push("  scrolled with no poke at all: not following the player".into());
                    continue;
                }
            }
            best = Some((player, plan, hold, out));
            break;
        }
    }
    let Some((player, plan, hold, best_out)) = best else {
        return Err(DiscoveryError::NothingFollows(report));
    };
    // W35-07: a walk that scrolled to the right is also tried to the left: a margin has two sides,
    // and a screen-flip game (the overworld's neighbours) scrolls both ways. A forward-only game
    // walks nowhere to the left (the extent stays under the floor) and keeps its one direction.
    let mut walks_left = false;
    if plan.strategy == ScoutStrategy::Walk {
        let mut left = plan.clone();
        left.dirs = vec![ScoutDir::Left];
        let l = run(&mut scout, left, &ctx)?;
        report.trials.push(format!(
            "{:?} {:?} no hold, Left: extent {} died {} after {} frames",
            plan.strategy,
            player.map(|p| p.lo),
            l.extent,
            l.died,
            l.frames
        ));
        walks_left = l.extent >= floor && !l.died;
    }

    // Tune the height of the winning hold: the HIGHEST that does about as well
    // as the best (the pipes and platforms are below the top of the screen, so
    // a lower hold catches on them, which a few cells' difference may not
    // show).
    let mut champion = hold.map(|h| (h, best_out));
    if let Some((hold, _)) = champion.clone() {
        let y = obs.ys.iter().find(|y| y.offset == hold.offset).copied();
        let wide = hold.pokes.len() > 1;
        if let Some(y) = y {
            let mut results: Vec<(Hold, Outcome)> = Vec::new();
            for delta in DELTAS {
                scout
                    .restart_with(plan.clone())
                    .map_err(|e| DiscoveryError::Machine(format!("{e:?}")))?;
                let pokes = hold_pokes(&scout, ctx.snes, wide, &y, delta);
                let mut trial = plan.clone();
                trial.hold = pokes.clone();
                trial.frame_budget = TUNE_FRAMES;
                trial.stall_frames = 120;
                let out = run(&mut scout, trial, &ctx)?;
                report.trials.push(format!(
                    "tune {:#x} delta {delta} {:?}: extent {} died {} after {} frames",
                    y.offset, pokes, out.extent, out.died, out.frames
                ));
                results.push((
                    Hold {
                        pokes,
                        delta,
                        offset: y.offset,
                        standing: y.standing,
                    },
                    out,
                ));
            }
            let top = results
                .iter()
                .filter(|(_, o)| !o.died)
                .map(|(_, o)| o.extent)
                .max()
                .unwrap_or(0);
            // `results` runs from the first height tried, then highest to
            // lowest.
            champion = results
                .into_iter()
                .filter(|(_, o)| !o.died && o.extent * 50 >= top * 49)
                .max_by_key(|(h, _)| h.delta);
        }
    }

    // W35-08: the level start has nothing to its left (Castlevania's camera sits at its left stop
    // there), so a Left that grew nothing from the start says nothing. Walk right first (the
    // winning recipe, hold included: Mega Man 2's walker dies in a pit without it), then try Left
    // from where the walk stands.
    if plan.strategy == ScoutStrategy::Walk && !walks_left {
        let mut walker = plan.clone();
        if let Some((h, _)) = champion.as_ref() {
            walker.hold = h.pokes.clone();
        }
        let mut right = walker.clone();
        right.dirs = vec![ScoutDir::Right];
        right.frame_budget = MIDWAY_FRAMES;
        let r = run(&mut scout, right, &ctx)?;
        if !r.died {
            let mut left = walker.clone();
            left.dirs = vec![ScoutDir::Left];
            let mut from_mid = scout
                .start_from_here(ctx.rom, left.clone(), ctx.size)
                .map_err(|e| DiscoveryError::Machine(format!("{e:?}")))?;
            let l = run(&mut from_mid, left, &ctx)?;
            report.trials.push(format!(
                "{:?} {:?} hold {:?}, Left from {} frames of Right: extent {} died {} after {} frames",
                walker.strategy,
                player.map(|p| p.lo),
                walker.hold,
                r.frames,
                l.extent,
                l.died,
                l.frames
            ));
            walks_left = l.extent >= floor && !l.died;
        } else {
            report.trials.push(format!(
                "{:?} Right for the midway Left trial died after {} frames: no Left verdict",
                walker.strategy, r.frames
            ));
        }
    }

    let vertical = confirm_y(
        &ctx,
        ctx.vertical.as_ref().or(main_room.as_ref()),
        obs,
        &plan,
        &mut report,
    )?;
    // The gates the winning recipe leaves true (and, where a vertical room was
    // seen, that hold there too).
    let mut final_plan = plan.clone();
    if let Some(h) = champion.as_ref() {
        final_plan.hold = h.0.pokes.clone();
    }
    final_plan.frame_budget = TRIAL_FRAMES;
    let survived = gate_survival(&mut scout, final_plan, &obs.gates, &ctx)?;
    let gates: Vec<GateCandidate> = obs
        .gates
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            survived.get(*i).is_some_and(|&f| f >= GATE_SURVIVAL)
                && vertical
                    .gates_hold
                    .as_ref()
                    .is_none_or(|h| h.get(*i).copied().unwrap_or(false))
        })
        .map(|(_, g)| *g)
        .collect();
    report.trials.push(format!(
        "gates kept {:?} of {:?}",
        gates.iter().map(|g| g.offset).collect::<Vec<_>>(),
        obs.gates.iter().map(|g| g.offset).collect::<Vec<_>>()
    ));
    // The later trials stay on the direction they were tuned in; only the table says "left" too.
    let mut shipped = plan.clone();
    if walks_left {
        shipped.dirs = vec![ScoutDir::Right, ScoutDir::Left];
    }
    let skeleton = skeleton(
        &ctx,
        obs,
        &gates,
        player,
        &shipped,
        champion.as_ref().map(|c| &c.0),
        vertical.y_word.as_ref(),
    );
    Ok((skeleton, report))
}

fn skeleton(
    ctx: &Context<'_>,
    obs: &Observed,
    gates: &[GateCandidate],
    player: Option<PlayerCandidate>,
    plan: &ScoutPlan,
    hold: Option<&Hold>,
    y_word: Option<&(YCandidate, bool, usize)>,
) -> Skeleton {
    let snes = ctx.snes;
    let mut rows: Vec<Row> = Vec::new();
    let mut confidence = 1.0f32;
    if let Some(c) = obs.camera.filter(|_| snes) {
        let wide = c.x.width == rf_enhance::camera_finder::Width::U16;
        rows.push(Row {
            role: "camera_x",
            addr: bus_address(snes, c.x.offset),
            value: None,
            notes: format!(
                "Held the scroll register's low byte on {:.0}% of {} scrolling frames{}.",
                c.x.ratio * 100.0,
                c.x.moving,
                if wide {
                    ", 16-bit"
                } else {
                    " (one byte: no carry seen)"
                }
            ),
            confidence: c.x.ratio,
        });
        confidence = confidence.min(c.x.ratio * if wide { 1.0 } else { 0.5 });
    }
    // Ticket W33-08: the vertical scene's camera, where the finder found one
    // (it watched the scroll register's Y and the word that followed it).
    if let Some(y) = obs.camera.filter(|_| snes).and_then(|c| c.y) {
        rows.push(Row {
            role: "camera_y",
            addr: bus_address(snes, y.offset),
            value: None,
            notes: format!(
                "Held the scroll register's Y low byte on {:.0}% of {} frames the screen scrolled vertically.",
                y.ratio * 100.0,
                y.moving
            ),
            confidence: y.ratio,
        });
    }
    if let Some(p) = player {
        rows.push(Row {
            role: "player_x",
            addr: bus_address(snes, p.lo),
            value: None,
            notes: format!(
                "Stepped with the camera on {:.0}% of {} scrolling frames, went up on Right and down on Left, and kept moving on {} frames the camera stood still; the scout moves the player with it and the camera followed.",
                p.step * 100.0,
                p.moving,
                p.wall
            ),
            confidence: p.confidence,
        });
        if let Some(hi) = p.hi.filter(|&h| h != p.lo + 1) {
            rows.push(Row {
                role: "player_x_hi",
                addr: bus_address(snes, hi),
                value: None,
                notes:
                    "High byte of the player X: stepped by one in the frames the low byte wrapped."
                        .to_string(),
                confidence: p.confidence,
            });
        }
        confidence = confidence.min(p.confidence);
    }
    if let Some(h) = hold {
        // The hold's bytes are the finder's Y candidate (a byte that moved
        // within 6 frames of a jump press): name them as the player's Y for a
        // vertical pass, which then releases the hold and moves the byte
        // itself. Only what was SEEN is claimed; a vertical trial has not
        // confirmed it (it can be a Y speed, not a position: Super Mario
        // World's candidate was `$7E007D`).
        for (i, &(addr, _)) in h.pokes.iter().enumerate().filter(|_| y_word.is_none()) {
            let seen = obs.ys.iter().find(|y| y.offset == h.offset);
            rows.push(Row {
                role: if i == 0 { "player_y" } else { "player_y_hi" },
                addr,
                value: None,
                notes: format!(
                    "{} of the finder's jump candidate: it read {} standing and moved within 6 frames of a jump press in {} of {} presses; the Y recipe is untried on it (a vertical pass has not been run).",
                    if i == 0 { "Low byte" } else { "High byte" },
                    h.standing,
                    seen.map_or(0, |y| y.hits),
                    seen.map_or(0, |y| y.windows)
                ),
                confidence: 0.5,
            });
        }
        for (i, &(addr, value)) in h.pokes.iter().enumerate() {
            rows.push(Row {
                role: "hold",
                addr,
                value: Some(value),
                notes: format!(
                    "{} of the player's Y: {} standing, held {} pixels above that; the scout kept going where the same scout with no hold was cut short.",
                    if i == 0 { "Low byte" } else { "High byte" },
                    h.standing,
                    h.delta
                ),
                confidence: 0.8,
            });
        }
    }
    if let Some((y, wide, extent)) = y_word {
        let screen = usize::from(ctx.size.0) * usize::from(ctx.size.1);
        for (i, role) in ["player_y", "player_y_hi"].into_iter().enumerate() {
            if i == 1 && !wide {
                break;
            }
            rows.push(Row {
                role,
                addr: bus_address(snes, y.offset) + i as u32,
                value: None,
                notes: if obs.top_down {
                    format!(
                        "{} of the player's Y word: it stepped with the camera's Y on {} of {} frames the screen scrolled vertically while the pad held Up or Down; a Down pass that moved it took the canvases to {:.1} screens.",
                        if i == 0 { "Low byte" } else { "High byte" },
                        y.hits,
                        y.windows,
                        *extent as f32 / screen as f32
                    )
                } else {
                    format!(
                        "{} of the player's Y word: it read {} standing and moved within 6 frames of a jump press in {} of {} presses; a Down pass that moved it from a room that scrolls vertically took the canvases to {:.1} screens.",
                        if i == 0 { "Low byte" } else { "High byte" },
                        y.standing,
                        y.hits,
                        y.windows,
                        *extent as f32 / screen as f32
                    )
                },
                confidence: 0.9,
            });
        }
    }
    for g in gates
        .iter()
        .take(2)
        .filter(|g: &&GateCandidate| g.confidence >= MIN_GATE_CONFIDENCE)
    {
        rows.push(Row {
            role: "start_when",
            addr: bus_address(snes, g.offset),
            value: Some(g.value),
            notes: if g.differs < 0.6 {
                // A guard (W35-05): the game's mode byte does not change in a pause, this does.
                format!(
                    "Read {} on {:.0}% of the level's frames and something else on {:.0}% of the {} frames of a pause (Start pressed, the picture frozen). Seen in one level of this session.",
                    g.value,
                    g.constancy * 100.0,
                    g.freeze_apart * 100.0,
                    g.frozen_frames
                )
            } else {
                format!(
                    "Read {} on {:.0}% of the level's frames and something else on {:.0}% of the frames outside it (title, menus, cuts); tells the session's states apart at {:.2}. Seen in one level of this session.",
                    g.value,
                    g.constancy * 100.0,
                    g.differs * 100.0,
                    g.mode_score
                )
            },
            confidence: g.confidence,
        });
    }
    let walk = plan.strategy == ScoutStrategy::Walk;
    Skeleton {
        strategy: match plan.strategy {
            ScoutStrategy::Walk => "walk",
            ScoutStrategy::PlayerMove => "player_move",
            ScoutStrategy::CameraWrite => "camera_write",
        },
        step_px: if walk { 1 } else { TRIAL_STEP_PX },
        frame_budget: 3000,
        // The trials' own patience (W35-03: 60 stalled Castlevania's scout at
        // its first frame; the camera starts to follow a few dozen frames in).
        stall_frames: plan.stall_frames,
        settle_frames: 60,
        dirs: if walk && plan.dirs.contains(&ScoutDir::Left) {
            vec!["right", "left"]
        } else if walk {
            vec!["right"]
        } else {
            vec!["right", "left"]
        },
        rows,
        confidence,
    }
}

/// The scout plan a discovered skeleton runs as (the same rows
/// `ScoutPlan::from_profile` reads from a profile).
#[must_use]
pub fn plan_of(sk: &Skeleton) -> ScoutPlan {
    let words = |role: &str| -> Vec<u32> {
        sk.rows
            .iter()
            .filter(|r| r.role == role)
            .map(|r| r.addr)
            .collect()
    };
    let valued = |role: &str| -> Vec<(u32, u8)> {
        sk.rows
            .iter()
            .filter(|r| r.role == role)
            .filter_map(|r| r.value.map(|v| (r.addr, v)))
            .collect()
    };
    ScoutPlan {
        strategy: match sk.strategy {
            "walk" => ScoutStrategy::Walk,
            "camera_write" => ScoutStrategy::CameraWrite,
            _ => ScoutStrategy::PlayerMove,
        },
        camera: words("camera_x"),
        player_x: words("player_x"),
        player_x_hi: words("player_x_hi"),
        camera_y: words("camera_y"),
        player_y: words("player_y"),
        player_y_hi: words("player_y_hi"),
        step_px: sk.step_px,
        frame_budget: sk.frame_budget,
        stall_frames: sk.stall_frames,
        settle_frames: sk.settle_frames,
        dirs: sk
            .dirs
            .iter()
            .map(|d| ScoutDir::parse(d).unwrap_or(ScoutDir::Right))
            .collect(),
        hold: valued("hold"),
        start_when: valued("start_when"),
        pin: false,
        scene_key: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_enhance::scout_finder::Row;

    /// `frames` frames of one pose (camera x walking `step` px a frame from `cam`), the gate
    /// reading `value` (held when it is 9).
    fn leg(out: &mut Vec<Sample>, frames: usize, cam: &mut u16, step: u16, value: u8) {
        for _ in 0..frames {
            *cam = cam.wrapping_add(step);
            out.push(Sample {
                pose: [*cam, 0],
                held: vec![value == 9],
            });
        }
    }

    #[test]
    fn a_gate_that_is_false_only_where_the_game_holds_the_player_survives() {
        // 80 frames walked, a speech box of 100 frozen frames (mode 0x0e), 60 walked again.
        let (mut run, mut cam) = (Vec::new(), 0u16);
        leg(&mut run, 80, &mut cam, 2, 9);
        leg(&mut run, 100, &mut cam, 0, 0x0e);
        leg(&mut run, 60, &mut cam, 2, 9);
        let judged = control_survival(&run, 1);
        assert!(judged[0] >= GATE_SURVIVAL, "{judged:?}");
        // The plain share is what dropped it (140 of 240).
        let plain = run.iter().filter(|s| s.held[0]).count() as f32 / run.len() as f32;
        assert!(plain < 0.6, "{plain}");
    }

    #[test]
    fn a_gate_that_is_false_while_the_game_scrolls_between_areas_survives() {
        // Walking, 20 frames of the game's own 8 px scroll (mode 0x0f), walking.
        let (mut run, mut cam) = (Vec::new(), 100u16);
        leg(&mut run, 80, &mut cam, 2, 9);
        leg(&mut run, 20, &mut cam, 8, 0x0f);
        leg(&mut run, 80, &mut cam, 2, 9);
        assert!(control_survival(&run, 1)[0] >= GATE_SURVIVAL);
    }

    #[test]
    fn a_gate_that_is_false_while_the_scout_walks_is_still_dropped() {
        // False on a third of the frames the camera walks at 2 px: no excuse.
        let (mut run, mut cam) = (Vec::new(), 0u16);
        leg(&mut run, 100, &mut cam, 2, 9);
        leg(&mut run, 50, &mut cam, 2, 3);
        leg(&mut run, 100, &mut cam, 2, 9);
        assert!(control_survival(&run, 1)[0] < GATE_SURVIVAL);
    }

    #[test]
    fn a_run_with_nothing_to_judge_is_judged_on_every_frame() {
        let (mut run, mut cam) = (Vec::new(), 0u16);
        leg(&mut run, 120, &mut cam, 0, 3);
        assert_eq!(control_survival(&run, 1), vec![0.0]);
        let mut held = Vec::new();
        leg(&mut held, 120, &mut cam, 0, 9);
        assert_eq!(control_survival(&held, 1), vec![1.0]);
    }

    #[test]
    fn finder_offsets_are_bus_addresses() {
        assert_eq!(bus_address(true, 0x1A), 0x7E_001A);
        assert_eq!(bus_address(false, 0x90), 0x90);
        // The NES finder's second range is the cartridge RAM window.
        assert_eq!(bus_address(false, 0x0800), 0x6000);
        assert_eq!(bus_address(false, 0x0800 + 0x10), 0x6010);
    }

    fn row(role: &'static str, addr: u32, value: Option<u8>) -> Row {
        Row {
            role,
            addr,
            value,
            notes: String::new(),
            confidence: 1.0,
        }
    }

    /// A skeleton runs as the same plan `ScoutPlan::from_profile` would make of
    /// its TOML.
    #[test]
    fn a_skeleton_runs_as_the_plan_its_profile_rows_make() {
        let sk = Skeleton {
            strategy: "player_move",
            step_px: 4,
            frame_budget: 3000,
            stall_frames: 60,
            settle_frames: 60,
            dirs: vec!["right", "left"],
            rows: vec![
                row("camera_x", 0x7E_001A, None),
                row("player_x", 0x7E_0094, None),
                row("hold", 0x7E_0096, Some(150)),
                row("hold", 0x7E_0097, Some(0)),
                row("start_when", 0x7E_0100, Some(0x14)),
            ],
            confidence: 0.9,
        };
        let plan = plan_of(&sk);
        let text = format!(
            "[meta]\nprofile_version = \"0.1\"\ntitle = \"T\"\nconsole = \"snes\"\nregion = \"ntsc\"\nauthors = [\"t\"]\nsources = []\n[[identity]]\nsha256 = \"{}\"\nrevision = \"t\"\n[capabilities]\nfull_level = false\nhud_separation = false\nentity_overlay = false\nwidescreen = \"decoded\"\n\n{}",
            "0".repeat(64),
            sk.to_toml()
        );
        let profile = rf_profiles::load_str(&text)
            .expect("the table loads")
            .profile;
        let from_profile = ScoutPlan::from_profile(profile.scout.as_ref().expect("[scout]"));
        assert_eq!(plan, from_profile);
        assert_eq!(plan.hold, vec![(0x7E_0096, 150), (0x7E_0097, 0)]);
        assert_eq!(plan.start_when, vec![(0x7E_0100, 0x14)]);
    }
}
