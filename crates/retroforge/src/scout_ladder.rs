//! Ticket W33-06: the scout's strategy ladder and its agreement check, as
//! pure rules (no machine, no thread), so every one is tested without a ROM.
//!
//! **The ladder** (`docs/design/STITCHING_PLAN.md` §3.2). A scene's scout tries
//! `player_move`, then `walk`, then `camera_write`, then gives up: margins show
//! live cells only. A rung is tried at a step (pixels per scout frame) that
//! starts at the profile's value (8 when it has none) and is halved, down to
//! [`STEP_FLOOR_PX`], whenever the check refuses it. A profile may pin its
//! strategy (no other rung is tried), and the rung that last worked for a
//! scene starts the next scout of it. A rung with nothing to act on is skipped:
//! `player_move` needs player addresses, `camera_write` camera addresses,
//! `walk` a console that places by bands (the NES).
//!
//! **The check** (§3.3). A delta's cells are compared with what the LIVE game
//! drew in the same places; [`check_delta`] turns the per-layer agreement into
//! one verdict (the worst layer decides, a layer with too few shared cells
//! abstains). The same function compares a scout with a slower scout (the
//! step probe, see `scout.rs`): the live canvas only ever holds the screen the
//! player stands on, and the corruption of a too-fast scout (Super Mario Bros.
//! 3 at 8 px per frame: the game cannot keep up) sits in the columns AHEAD of
//! it, where there is nothing live to compare with.

use rf_enhance::canvas::{scout_agreement_aligned, scout_verdict, ScoutVerdict, LAYERS};
use rf_enhance::persistence::ScoutMemo;
use rf_enhance::stitcher::Canvas;

use crate::scout::{ScoutAxis, ScoutDir, ScoutEnd, ScoutPlan, ScoutStrategy};

/// Step a rung starts at when the profile has none.
pub const DEFAULT_STEP_PX: u16 = 8;
/// The smallest step the ladder tries (a profile with a smaller step keeps
/// it as its own floor).
pub const STEP_FLOOR_PX: u16 = 2;
/// A pass at a smaller step runs up to this many times the frame budget, so
/// it still covers the distance the profile's budget was written for.
pub const MAX_BUDGET_SCALE: u32 = 4;

/// The memo code of a rung ([`ScoutMemo::rung`]).
#[must_use]
pub fn rung_code(s: ScoutStrategy) -> u8 {
    match s {
        ScoutStrategy::PlayerMove => ScoutMemo::PLAYER_MOVE,
        ScoutStrategy::Walk => ScoutMemo::WALK,
        ScoutStrategy::CameraWrite => ScoutMemo::CAMERA_WRITE,
    }
}

/// The rung of a memo code (`None` for "gave up" and unknown codes).
#[must_use]
pub fn rung_of_code(code: u8) -> Option<ScoutStrategy> {
    match code {
        ScoutMemo::PLAYER_MOVE => Some(ScoutStrategy::PlayerMove),
        ScoutMemo::WALK => Some(ScoutStrategy::Walk),
        ScoutMemo::CAMERA_WRITE => Some(ScoutStrategy::CameraWrite),
        _ => None,
    }
}

/// The name a profile and the logs use for a rung.
#[must_use]
pub fn rung_name(s: ScoutStrategy) -> &'static str {
    match s {
        ScoutStrategy::PlayerMove => "player_move",
        ScoutStrategy::Walk => "walk",
        ScoutStrategy::CameraWrite => "camera_write",
    }
}

/// The rungs a scout may try on `axis`, in order. `bands`: the machine is
/// placed by the core's own scroll events (the NES), the only place `walk` can
/// act. `hint`: the rung that worked last time on this scene (tried first).
/// The profile's strategy is the recipe of the X axis; a Y pass starts at
/// `player_move` (a game that scrolls sideways on a held pad does not scroll
/// down on one).
#[must_use]
pub fn rungs_for(
    plan: &ScoutPlan,
    axis: ScoutAxis,
    bands: bool,
    hint: Option<ScoutStrategy>,
) -> Vec<ScoutStrategy> {
    let usable = |s: ScoutStrategy| match s {
        ScoutStrategy::PlayerMove => !plan.player_words(axis).is_empty(),
        ScoutStrategy::Walk => bands,
        ScoutStrategy::CameraWrite => !plan.camera_words(axis).is_empty(),
    };
    if plan.pin {
        // The profile's word is final.
        return vec![plan.strategy];
    }
    let canonical = [
        ScoutStrategy::PlayerMove,
        ScoutStrategy::Walk,
        ScoutStrategy::CameraWrite,
    ];
    let own = match axis {
        ScoutAxis::X => plan.strategy,
        ScoutAxis::Y => ScoutStrategy::PlayerMove,
    };
    let first = hint.filter(|h| usable(*h)).unwrap_or(own);
    let mut out = Vec::with_capacity(canonical.len());
    for s in std::iter::once(first).chain(canonical) {
        if usable(s) && !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// Where the ladder stands: which rung, at which step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ladder {
    rungs: Vec<ScoutStrategy>,
    at: usize,
    step: u16,
    start_step: u16,
    floor: u16,
}

/// What [`Ladder::reject`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    /// The same rung at half the step.
    Halved(u16),
    /// The next rung, at the starting step.
    NextRung(ScoutStrategy),
    /// Nothing left to try.
    GaveUp,
}

impl Ladder {
    /// A ladder over `rungs` starting at `start_step` (`0` = the default).
    #[must_use]
    pub fn new(rungs: Vec<ScoutStrategy>, start_step: u16) -> Self {
        let start = if start_step == 0 {
            DEFAULT_STEP_PX
        } else {
            start_step
        };
        Self {
            rungs,
            at: 0,
            step: start,
            start_step: start,
            floor: STEP_FLOOR_PX.min(start),
        }
    }

    /// The rung and step to try now; `None` = given up.
    #[must_use]
    pub fn current(&self) -> Option<(ScoutStrategy, u16)> {
        self.rungs.get(self.at).map(|&s| (s, self.step))
    }

    #[must_use]
    pub fn gave_up(&self) -> bool {
        self.at >= self.rungs.len()
    }

    /// Whether the step means anything here and can still go down (`walk`
    /// moves at the game's own pace).
    #[must_use]
    pub fn tunable(&self) -> bool {
        matches!(self.current(), Some((s, step)) if s != ScoutStrategy::Walk && step > self.floor)
    }

    /// The step the current one is checked against: half of it.
    #[must_use]
    pub fn reference_step(&self) -> Option<u16> {
        self.tunable().then(|| (self.step / 2).max(self.floor))
    }

    /// The frame budget of a pass at the current step: the profile's, scaled
    /// by how much slower than its own step the pass runs.
    #[must_use]
    pub fn scaled_budget(&self, base: u32) -> u32 {
        let scale = u32::from(self.start_step / self.step.max(1)).clamp(1, MAX_BUDGET_SCALE);
        base.saturating_mul(scale)
    }

    /// The check refused the current rung at the current step: half the step
    /// while there is room, else the next rung at the starting step.
    pub fn reject(&mut self) -> Moved {
        if self.tunable() {
            self.step = (self.step / 2).max(self.floor);
            return Moved::Halved(self.step);
        }
        self.at += 1;
        self.step = self.start_step;
        self.current()
            .map_or(Moved::GaveUp, |(s, _)| Moved::NextRung(s))
    }

    /// The check said "slow" (95-99 %): lower the step for what comes next.
    /// Returns whether it moved.
    pub fn lower(&mut self) -> bool {
        if self.tunable() {
            self.step = (self.step / 2).max(self.floor);
            true
        } else {
            false
        }
    }

    /// The memo to persist for a scout that finished on this ladder's
    /// current rung.
    #[must_use]
    pub fn memo(&self) -> Option<ScoutMemo> {
        self.current().map(|(s, step)| ScoutMemo {
            rung: rung_code(s),
            step_px: u8::try_from(step).unwrap_or(u8::MAX),
        })
    }
}

/// What the check made of one delta (or one probe).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeltaCheck {
    /// Cells that agree, over the layers that could be judged.
    pub agreed: usize,
    /// Cells both sides know, over the layers that could be judged.
    pub shared: usize,
    /// The worst layer's verdict; `None` = no layer had cells enough.
    pub verdict: Option<ScoutVerdict>,
    /// The vertical offset the best layer was read at (0 = none).
    pub dy: i64,
}

impl DeltaCheck {
    /// Agreement in thousandths (`None` when nothing was judged).
    #[must_use]
    pub fn permille(&self) -> Option<u32> {
        (self.shared > 0).then(|| (self.agreed * 1000 / self.shared) as u32)
    }
}

/// How bad a verdict is, for "the worst layer decides".
fn severity(v: ScoutVerdict) -> u8 {
    match v {
        ScoutVerdict::Merge => 0,
        ScoutVerdict::SlowDown => 1,
        ScoutVerdict::Reject => 2,
    }
}

/// Compare `delta` with `reference` layer by layer (`trusted` = the layers
/// whose canvases are worth comparing with).
#[must_use]
pub fn check_delta(
    reference: &[Canvas; LAYERS],
    delta: &[Canvas; LAYERS],
    trusted: [bool; LAYERS],
) -> DeltaCheck {
    let mut out = DeltaCheck::default();
    for ((live, scout), ok) in reference.iter().zip(delta).zip(trusted) {
        if !ok {
            continue;
        }
        let a = scout_agreement_aligned(live, scout, None);
        let Some(v) = scout_verdict((a.agreed, a.shared)) else {
            continue;
        };
        out.agreed += a.agreed;
        out.shared += a.shared;
        if out.verdict.is_none_or(|w| severity(v) > severity(w)) {
            out.verdict = Some(v);
            out.dy = a.dy;
        }
    }
    out
}

/// One thing the check looked at, for the log and the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attempt {
    /// `"probe"` (a scout against a slower scout) or `"delta"` (a delta
    /// against the live canvas).
    pub what: &'static str,
    pub strategy: ScoutStrategy,
    pub step_px: u16,
    pub check: DeltaCheck,
}

/// What became of one pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassReport {
    pub dir: ScoutDir,
    /// The rung and step the pass finished on (`None`: no rung worked).
    pub finished_on: Option<(ScoutStrategy, u16)>,
    pub end: Option<ScoutEnd>,
}

/// What a scout did, for the telemetry and the log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScoutReport {
    pub attempts: Vec<Attempt>,
    pub passes: Vec<PassReport>,
    /// Every rung failed the check on the first pass.
    pub gave_up: bool,
    /// Live cells (all trusted layers) the checks had to compare with when
    /// the scout started: a scene the live game has barely drawn can judge
    /// nothing.
    pub reference_cells: usize,
}

impl ScoutReport {
    /// The rung and step the first pass that worked finished on.
    #[must_use]
    pub fn winner(&self) -> Option<(ScoutStrategy, u16)> {
        self.passes.iter().find_map(|p| p.finished_on)
    }

    /// One line: the verdicts and the rung, agreement as a percentage.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut s = String::new();
        for a in &self.attempts {
            let pct = a.check.permille().map_or_else(
                || "n/a".to_string(),
                |p| format!("{:.1}%", f64::from(p) / 10.0),
            );
            let v = match a.check.verdict {
                Some(ScoutVerdict::Merge) => "merge",
                Some(ScoutVerdict::SlowDown) => "slow",
                Some(ScoutVerdict::Reject) => "REJECT",
                None => "unjudged",
            };
            s.push_str(&format!(
                " [{} {}@{}px {pct} {v}]",
                a.what,
                rung_name(a.strategy),
                a.step_px
            ));
        }
        match self.winner() {
            Some((r, step)) => s.push_str(&format!(" -> {}@{step}px", rung_name(r))),
            None => s.push_str(" -> gave up"),
        }
        s.push_str(&format!(
            " (live cells to compare with: {})",
            self.reference_cells
        ));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::{PixelLayer, PpuPixel};
    use rf_enhance::canvas::MIN_SHARED_CELLS;

    fn plan(strategy: ScoutStrategy, player: bool, camera: bool, pin: bool) -> ScoutPlan {
        ScoutPlan {
            strategy,
            camera: if camera { vec![0x1A] } else { Vec::new() },
            player_x: if player { vec![0x94] } else { Vec::new() },
            player_x_hi: Vec::new(),
            camera_y: Vec::new(),
            player_y: Vec::new(),
            player_y_hi: Vec::new(),
            step_px: 8,
            frame_budget: 100,
            stall_frames: 10,
            settle_frames: 0,
            dirs: vec![ScoutDir::Right],
            hold: Vec::new(),
            start_when: Vec::new(),
            pin,
            scene_key: None,
            reach_px: 0,
        }
    }

    use ScoutStrategy::{CameraWrite, PlayerMove, Walk};

    #[test]
    fn the_ladder_runs_player_move_walk_camera_write_skipping_rungs_with_nothing_to_act_on() {
        // A SNES game with both address sets: no walk (the console is not
        // placed by bands).
        let p = plan(PlayerMove, true, true, false);
        assert_eq!(
            rungs_for(&p, ScoutAxis::X, false, None),
            vec![PlayerMove, CameraWrite]
        );
        // The NES with player addresses only (SMB3): walk is a real fallback.
        let p = plan(PlayerMove, true, false, false);
        assert_eq!(
            rungs_for(&p, ScoutAxis::X, true, None),
            vec![PlayerMove, Walk]
        );
        // MM2: walk first (the profile's), the rest follow.
        let p = plan(Walk, false, false, false);
        assert_eq!(rungs_for(&p, ScoutAxis::X, true, None), vec![Walk]);
        let p = plan(Walk, true, true, false);
        assert_eq!(
            rungs_for(&p, ScoutAxis::X, true, None),
            vec![Walk, PlayerMove, CameraWrite]
        );
    }

    #[test]
    fn a_vertical_scene_starts_at_player_move_whatever_the_profiles_x_strategy() {
        // MM2: the profile says walk (right on the pad); a held Down does
        // nothing for a game that scrolls down only when the player falls.
        let mut p = plan(Walk, false, false, false);
        p.player_y = vec![0x04A0];
        assert_eq!(rungs_for(&p, ScoutAxis::X, true, None), vec![Walk]);
        assert_eq!(
            rungs_for(&p, ScoutAxis::Y, true, None),
            vec![PlayerMove, Walk]
        );
        // No Y words and no pad: nothing to try.
        let p = plan(PlayerMove, true, true, false);
        assert!(rungs_for(&p, ScoutAxis::Y, false, None).is_empty());
    }

    #[test]
    fn a_pinned_profile_tries_its_own_strategy_and_nothing_else() {
        let p = plan(Walk, true, true, true);
        assert_eq!(
            rungs_for(&p, ScoutAxis::X, true, Some(PlayerMove)),
            vec![Walk]
        );
    }

    #[test]
    fn the_rung_that_worked_last_time_goes_first() {
        let p = plan(PlayerMove, true, true, false);
        assert_eq!(
            rungs_for(&p, ScoutAxis::X, true, Some(CameraWrite)),
            vec![CameraWrite, PlayerMove, Walk]
        );
        // A remembered rung this profile cannot act on is ignored.
        let p = plan(PlayerMove, true, false, false);
        assert_eq!(
            rungs_for(&p, ScoutAxis::X, false, Some(CameraWrite)),
            vec![PlayerMove]
        );
    }

    #[test]
    fn a_rejected_step_halves_to_the_floor_then_the_next_rung_then_gives_up() {
        let mut l = Ladder::new(vec![PlayerMove, CameraWrite], 8);
        assert_eq!(l.current(), Some((PlayerMove, 8)));
        assert_eq!(l.reject(), Moved::Halved(4));
        assert_eq!(l.reject(), Moved::Halved(2));
        // At the floor: the next rung, back at the starting step.
        assert_eq!(l.reject(), Moved::NextRung(CameraWrite));
        assert_eq!(l.current(), Some((CameraWrite, 8)));
        assert_eq!(l.reject(), Moved::Halved(4));
        assert_eq!(l.reject(), Moved::Halved(2));
        assert_eq!(l.reject(), Moved::GaveUp);
        assert!(l.gave_up() && l.current().is_none());
    }

    #[test]
    fn the_step_starts_at_eight_without_a_profile_value_and_a_small_profile_step_is_its_own_floor()
    {
        assert_eq!(
            Ladder::new(vec![PlayerMove], 0).current(),
            Some((PlayerMove, 8))
        );
        let mut one = Ladder::new(vec![PlayerMove], 1);
        assert!(!one.tunable(), "1 px has nowhere lower to go");
        assert_eq!(one.reject(), Moved::GaveUp);
        let mut three = Ladder::new(vec![PlayerMove], 3);
        assert_eq!(three.reject(), Moved::Halved(2));
    }

    #[test]
    fn walk_has_no_step_to_tune() {
        let mut l = Ladder::new(vec![Walk, PlayerMove], 8);
        assert!(!l.tunable() && l.reference_step().is_none());
        assert_eq!(l.reject(), Moved::NextRung(PlayerMove));
    }

    #[test]
    fn slow_lowers_the_step_without_changing_rung_and_stops_at_the_floor() {
        let mut l = Ladder::new(vec![PlayerMove], 4);
        assert!(l.lower());
        assert_eq!(l.current(), Some((PlayerMove, 2)));
        assert!(!l.lower(), "already at the floor");
        assert_eq!(l.current(), Some((PlayerMove, 2)));
    }

    #[test]
    fn the_probe_reference_is_half_the_step_and_the_budget_scales_with_it() {
        let mut l = Ladder::new(vec![PlayerMove], 8);
        assert_eq!(l.reference_step(), Some(4));
        assert_eq!(l.scaled_budget(1500), 1500);
        l.reject();
        assert_eq!(l.scaled_budget(1500), 3000);
        l.reject();
        assert_eq!(l.scaled_budget(1500), 6000);
        assert_eq!(
            l.reference_step(),
            None,
            "the floor has no slower reference"
        );
        // Capped: a profile step of 64 at the floor is x4, not x32.
        let mut big = Ladder::new(vec![PlayerMove], 64);
        while big.tunable() {
            big.reject();
        }
        assert_eq!(big.scaled_budget(10), 40);
    }

    #[test]
    fn the_memo_names_the_rung_and_step_and_round_trips_through_its_codes() {
        let mut l = Ladder::new(vec![PlayerMove, Walk], 8);
        l.reject();
        let m = l.memo().expect("a rung");
        assert_eq!((m.rung, m.step_px), (ScoutMemo::PLAYER_MOVE, 4));
        for s in [PlayerMove, Walk, CameraWrite] {
            assert_eq!(rung_of_code(rung_code(s)), Some(s));
        }
        assert_eq!(rung_of_code(ScoutMemo::GAVE_UP), None);
    }

    fn px(i: u8) -> PpuPixel {
        PpuPixel {
            palette_index: i,
            layer: PixelLayer::Background(0),
            sprite_id: None,
            priority: 0,
        }
    }

    /// A `w` x `h` canvas at the origin whose cell (x, y) is `f(x, y)`.
    fn canvas(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> Canvas {
        let mut c = Canvas::new();
        for y in 0..h {
            let row: Vec<_> = (0..w).map(|x| Some(px(f(x, y)))).collect();
            c.blit_row(0, y as i64, &row);
        }
        c
    }

    fn layers(c: Canvas) -> [Canvas; LAYERS] {
        let mut out: [Canvas; LAYERS] = Default::default();
        out[0] = c;
        out
    }

    #[test]
    fn identical_cells_merge_and_a_few_wrong_ones_slow_and_many_reject() {
        // 100 x 40 = 4000 shared cells.
        let base = |x: usize, y: usize| ((x * 7 + y * 13) % 50) as u8;
        let live = layers(canvas(100, 40, base));
        let same = check_delta(&live, &layers(canvas(100, 40, base)), [true; LAYERS]);
        assert_eq!(same.verdict, Some(ScoutVerdict::Merge));
        assert_eq!((same.agreed, same.shared), (4000, 4000));
        // 3 % of the rows recoloured: 95-99 %.
        let slow = check_delta(
            &live,
            &layers(canvas(100, 40, |x, y| if y == 5 { 99 } else { base(x, y) })),
            [true; LAYERS],
        );
        assert_eq!(slow.verdict, Some(ScoutVerdict::SlowDown));
        // A quarter of the columns wrong: under 95 %.
        let bad = check_delta(
            &live,
            &layers(canvas(
                100,
                40,
                |x, y| if x % 4 == 0 { 99 } else { base(x, y) },
            )),
            [true; LAYERS],
        );
        assert_eq!(bad.verdict, Some(ScoutVerdict::Reject));
        assert_eq!(bad.permille(), Some(750));
    }

    #[test]
    fn too_few_shared_cells_abstain_and_an_untrusted_layer_is_not_judged() {
        let base = |x: usize, y: usize| ((x + y) % 9) as u8;
        let live = layers(canvas(30, 30, base));
        // 900 shared cells < 2000: no verdict, however wrong.
        let few = check_delta(&live, &layers(canvas(30, 30, |_, _| 77)), [true; LAYERS]);
        assert_eq!(few.verdict, None);
        // A layer that is not trusted is skipped even with many cells.
        let big_live = layers(canvas(100, 40, base));
        let wrong = layers(canvas(100, 40, |_, _| 77));
        let skipped = check_delta(&big_live, &wrong, [false; LAYERS]);
        assert_eq!(skipped.verdict, None);
        assert_eq!(MIN_SHARED_CELLS, 2000);
    }

    #[test]
    fn the_worst_layer_decides() {
        let base = |x: usize, y: usize| ((x * 3 + y) % 40) as u8;
        let mut live: [Canvas; LAYERS] = Default::default();
        let mut delta: [Canvas; LAYERS] = Default::default();
        live[0] = canvas(100, 40, base);
        delta[0] = canvas(100, 40, base);
        live[1] = canvas(100, 40, base);
        delta[1] = canvas(100, 40, |_, _| 77);
        let c = check_delta(&live, &delta, [true; LAYERS]);
        assert_eq!(c.verdict, Some(ScoutVerdict::Reject));
    }

    #[test]
    fn a_scout_one_row_off_the_live_canvas_is_read_at_that_offset() {
        // The scout is the live picture shifted up one row (NES, SMB3 1-1).
        let pic = |x: usize, y: usize| ((x * 5 + y * y) % 60) as u8;
        let live = layers(canvas(100, 40, pic));
        let scout = layers(canvas(100, 39, |x, y| pic(x, y + 1)));
        let c = check_delta(&live, &scout, [true; LAYERS]);
        assert_eq!(c.verdict, Some(ScoutVerdict::Merge));
        assert_eq!(c.dy, 1);
    }

    #[test]
    fn scout_cells_of_the_live_canvas_are_not_evidence() {
        let base = |x: usize, y: usize| ((x * 7 + y * 13) % 50) as u8;
        let mut live = layers(canvas(100, 40, base));
        // Every cell of layer 0 was put there by an earlier scout merge.
        let mut tagged = Canvas::new();
        for y in 0..40 {
            let row: Vec<_> = (0..100).map(|x| Some(px(base(x, y)))).collect();
            tagged.blit_row_scout(0, y as i64, &row);
        }
        live[0] = tagged;
        let c = check_delta(&live, &layers(canvas(100, 40, |_, _| 77)), [true; LAYERS]);
        assert_eq!(c.verdict, None, "no live cell to compare with");
    }

    #[test]
    fn the_summary_lists_each_check_and_the_winning_rung() {
        let r = ScoutReport {
            attempts: vec![
                Attempt {
                    what: "probe",
                    strategy: PlayerMove,
                    step_px: 8,
                    check: DeltaCheck {
                        agreed: 90,
                        shared: 100,
                        verdict: Some(ScoutVerdict::Reject),
                        dy: 0,
                    },
                },
                Attempt {
                    what: "delta",
                    strategy: PlayerMove,
                    step_px: 4,
                    check: DeltaCheck {
                        agreed: 996,
                        shared: 1000,
                        verdict: Some(ScoutVerdict::Merge),
                        dy: 1,
                    },
                },
            ],
            passes: vec![PassReport {
                dir: ScoutDir::Right,
                finished_on: Some((PlayerMove, 4)),
                end: Some(ScoutEnd::Stalled),
            }],
            gave_up: false,
            reference_cells: 0,
        };
        let s = r.summary();
        assert!(s.contains("probe player_move@8px 90.0% REJECT"), "{s}");
        assert!(s.contains("delta player_move@4px 99.6% merge"), "{s}");
        assert!(s.contains("-> player_move@4px"), "{s}");
    }
}
