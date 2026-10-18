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
/// Y candidates tried.
pub const MAX_Y: usize = 16;
/// A trial that grew the canvases to less than this many screens did not
/// follow the player anywhere.
const MIN_SCREENS: usize = 2;

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
    Ok(Outcome {
        extent: scout.extent(),
        died,
        frames: scout.frames_run(),
    })
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
    let mut scout = Scout::start(ctx.rom, snapshot, base.clone(), seed, ctx.scene, ctx.size)
        .map_err(|e| DiscoveryError::Machine(format!("{e:?}")))?;
    let bands = scout.places_by_bands();
    let (w, h) = ctx.size;
    let floor = MIN_SCREENS * usize::from(w) * usize::from(h);

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
        movers.push((None, walk));
    }
    for p in obs.players.iter().skip(1).take(2) {
        movers.push((Some(*p), with_player(base.clone(), ctx.snes, p)));
    }
    type Winner = (Option<PlayerCandidate>, ScoutPlan, Option<Hold>, Outcome);
    let mut best: Option<Winner> = None;
    for (player, mut plan) in movers {
        plan.frame_budget = TRIAL_FRAMES;
        let none = run(&mut scout, plan.clone(), &ctx)?;
        report.trials.push(format!(
            "{:?} {:?} no hold: extent {} died {} after {} frames",
            plan.strategy,
            player.map(|p| p.lo),
            none.extent,
            none.died,
            none.frames
        ));
        let mut tried: Vec<(Option<Hold>, Outcome)> = vec![(None, none)];
        for y in obs.ys.iter().take(MAX_Y) {
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
            best = Some((player, plan, hold, out));
            break;
        }
    }
    let Some((player, plan, hold, best_out)) = best else {
        return Err(DiscoveryError::NothingFollows(report));
    };

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

    let skeleton = skeleton(&ctx, obs, player, &plan, champion.as_ref().map(|c| &c.0));
    Ok((skeleton, report))
}

fn skeleton(
    ctx: &Context<'_>,
    obs: &Observed,
    player: Option<PlayerCandidate>,
    plan: &ScoutPlan,
    hold: Option<&Hold>,
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
    for g in obs
        .gates
        .iter()
        .take(2)
        .filter(|g: &&GateCandidate| g.confidence >= 0.9)
    {
        rows.push(Row {
            role: "start_when",
            addr: bus_address(snes, g.offset),
            value: Some(g.value),
            notes: format!(
                "Read {} on {:.0}% of the level's frames and something else on {:.0}% of the frames outside it (title, menus, cuts). Seen in one level of this session.",
                g.value,
                g.constancy * 100.0,
                g.differs * 100.0
            ),
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
        stall_frames: 60,
        settle_frames: 60,
        dirs: if walk {
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
        step_px: sk.step_px,
        frame_budget: sk.frame_budget,
        stall_frames: sk.stall_frames,
        settle_frames: sk.settle_frames,
        dirs: sk
            .dirs
            .iter()
            .map(|d| {
                if *d == "left" {
                    ScoutDir::Left
                } else {
                    ScoutDir::Right
                }
            })
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
