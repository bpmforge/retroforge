//! No-latency extrapolation: the tick-hook half (ticket W39-01).
//!
//! Interpolation ([`crate::interpolate_snes::compose_snes`]) needs two finished frames and shows the
//! older one, so it adds a frame of input latency. Extrapolation shows the CURRENT frame and moves it
//! on, so it adds none; what it pays instead is the risk that motion changes right after the tick.
//! This module is the judge of that risk. At the game's tick (the end of its logic for the frame: the
//! NMI exit hook of a recipe, or the frame boundary where no hook host is installed) the caller reads
//! the recipe's RAM rows (camera, player, object slots) into [`TickFacts`] and hands them to
//! [`Tracker::tick`]. The tracker keeps the facts of the last tick, derives each point's velocity from
//! the last two ticks, and answers [`Verdict::Extrapolate`] only when moving on is safe:
//!
//! * not the first tick, not a gap in the frame count (a rewind, a pause): [`Refusal::NotConsecutive`];
//! * same scene (the recipe's game mode and level id): else [`Refusal::SceneCut`];
//! * the player and the camera moved no further than motion does: else [`Refusal::Teleport`];
//! * their velocity did not change sharply since the tick before (a reversal, a bounce): else
//!   [`Refusal::SharpChange`]. The refusal is for THIS tick only; the new velocity is remembered, so
//!   the next steady tick extrapolates again.
//!
//! Objects are judged the same way but only counted ([`Motion::objects_snapped`]): the pixels of an
//! object that teleported or turned are held by the composer (`interpolate::entry_deltas` refuses
//! steps beyond `MAX_SPRITE_STEP`), so one bad object does not take the whole picture back.
//!
//! The tracker only reads. Nothing here is fed back to the machine (law 6).
//!
//! Law 8: every walk is a `for` over an iterator.

use crate::interpolation::Refusal;

/// The camera moved further than this many dots in one tick: not motion (the same bound as
/// [`crate::interpolate::MAX_SCROLL_STEP`]).
pub const MAX_CAMERA_STEP: i32 = crate::interpolate::MAX_SCROLL_STEP;
/// The player or an object moved further than this in one tick: not the same object
/// ([`crate::interpolate::MAX_SPRITE_STEP`]).
pub const MAX_POINT_STEP: i32 = crate::interpolate::MAX_SPRITE_STEP;
/// A velocity component that changes by more than this between consecutive ticks is a sharp change.
pub const MAX_JERK: i32 = 4;

/// A position as the game keeps it: two 16-bit coordinates.
pub type Pos = (u16, u16);

/// What the tick hook read from the recipe's RAM rows at the end of one tick.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TickFacts {
    /// Completed-frame count the facts belong to.
    pub index: u64,
    /// Identity of the scene (the recipe's game mode and level id, packed by the reader).
    pub scene: u64,
    pub camera: Pos,
    pub player: Option<Pos>,
    /// One entry per object slot, `None` for a slot that is not in use.
    pub objects: Vec<Option<Pos>>,
}

/// How far the tracked things moved in the tick just seen, and what the tracker made of the objects.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Motion {
    pub camera: (i32, i32),
    pub player: Option<(i32, i32)>,
    /// Object slots that moved smoothly.
    pub objects_moving: u16,
    /// Object slots that jumped or changed velocity sharply (their pixels are held, not moved on).
    pub objects_snapped: u16,
}

/// What to do with the gap after this tick's frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Move the current frame on at the velocity it just had.
    Extrapolate(Motion),
    /// Show the current frame as it is.
    Snap(Refusal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Velocity {
    camera: (i32, i32),
    player: Option<(i32, i32)>,
    objects: Vec<Option<(i32, i32)>>,
}

/// Signed 16-bit difference `b - a`, so a coordinate that wraps is a small step, not a jump.
fn delta(a: Pos, b: Pos) -> (i32, i32) {
    let d = |p: u16, q: u16| i32::from(q.wrapping_sub(p) as i16);
    (d(a.0, b.0), d(a.1, b.1))
}

fn beyond(v: (i32, i32), limit: i32) -> bool {
    v.0.abs() > limit || v.1.abs() > limit
}

/// Whether `v` differs from `old` by more than [`MAX_JERK`] on either axis.
fn jerked(v: (i32, i32), old: (i32, i32)) -> bool {
    (v.0 - old.0).abs() > MAX_JERK || (v.1 - old.1).abs() > MAX_JERK
}

/// Keeps the last tick's facts and velocity; one per running game.
#[derive(Debug, Clone, Default)]
pub struct Tracker {
    last: Option<TickFacts>,
    velocity: Option<Velocity>,
}

impl Tracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything (a new game, a rewind the caller knows about).
    pub fn reset(&mut self) {
        self.last = None;
        self.velocity = None;
    }

    /// Take the facts of the tick that just ended and say what to do with the gap after it.
    pub fn tick(&mut self, facts: TickFacts) -> Verdict {
        let Some(prev) = self.last.replace(facts.clone()) else {
            self.velocity = None;
            return Verdict::Snap(Refusal::NotConsecutive);
        };
        if facts.index != prev.index.wrapping_add(1) {
            self.velocity = None;
            return Verdict::Snap(Refusal::NotConsecutive);
        }
        if facts.scene != prev.scene {
            self.velocity = None;
            return Verdict::Snap(Refusal::SceneCut);
        }
        let camera = delta(prev.camera, facts.camera);
        let player = match (prev.player, facts.player) {
            (Some(a), Some(b)) => Some(delta(a, b)),
            _ => None,
        };
        if beyond(camera, MAX_CAMERA_STEP) || player.is_some_and(|p| beyond(p, MAX_POINT_STEP)) {
            self.velocity = None;
            return Verdict::Snap(Refusal::Teleport);
        }
        let objects: Vec<Option<(i32, i32)>> = prev
            .objects
            .iter()
            .zip(&facts.objects)
            .map(|(a, b)| match (a, b) {
                (Some(a), Some(b)) => Some(delta(*a, *b)),
                _ => None,
            })
            .collect();

        // A slot that jumped has no velocity to remember: the tick after a jump is judged afresh.
        let jumped: Vec<bool> = objects
            .iter()
            .map(|v| v.is_some_and(|v| beyond(v, MAX_POINT_STEP)))
            .collect();
        let now = Velocity {
            camera,
            player,
            objects: objects
                .iter()
                .zip(&jumped)
                .map(|(v, j)| if *j { None } else { *v })
                .collect(),
        };
        let old = self.velocity.replace(now.clone());
        let mut motion = Motion {
            camera,
            player,
            ..Motion::default()
        };
        for (i, (v, jump)) in objects.iter().zip(&jumped).enumerate() {
            let Some(v) = v else { continue };
            let before = old
                .as_ref()
                .and_then(|o| o.objects.get(i).copied().flatten());
            if *jump || before.is_some_and(|b| jerked(*v, b)) {
                motion.objects_snapped += 1;
            } else if *v != (0, 0) {
                motion.objects_moving += 1;
            }
        }
        if let Some(old) = old {
            let player_jerk = match (player, old.player) {
                (Some(p), Some(o)) => jerked(p, o),
                _ => false,
            };
            if jerked(camera, old.camera) || player_jerk {
                return Verdict::Snap(Refusal::SharpChange);
            }
        }
        Verdict::Extrapolate(motion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(index: u64, cam: u16, player: u16) -> TickFacts {
        TickFacts {
            index,
            scene: 7,
            camera: (cam, 0),
            player: Some((player, 100)),
            objects: vec![Some((50, 60)), None],
        }
    }

    fn run(ticks: &[(u16, u16)]) -> Vec<Verdict> {
        let mut t = Tracker::new();
        ticks
            .iter()
            .enumerate()
            .map(|(i, (c, p))| t.tick(facts(i as u64, *c, *p)))
            .collect()
    }

    #[test]
    fn the_first_tick_has_no_velocity_and_the_second_has_no_history_to_jerk_against() {
        let v = run(&[(0, 10), (2, 12), (4, 14)]);
        assert_eq!(v[0], Verdict::Snap(Refusal::NotConsecutive));
        let Verdict::Extrapolate(m) = &v[1] else {
            panic!("second tick: {:?}", v[1]);
        };
        assert_eq!(m.camera, (2, 0));
        assert_eq!(m.player, Some((2, 0)));
        assert!(matches!(v[2], Verdict::Extrapolate(_)));
    }

    #[test]
    fn a_hard_reversal_snaps_for_that_tick_only() {
        // Running right at 3, then left at 3 (a change of 6), then steady left.
        let v = run(&[
            (0, 10),
            (3, 13),
            (6, 16),
            (3, 13),
            (0, 10),
            (-3i16 as u16, 7),
        ]);
        assert!(matches!(v[2], Verdict::Extrapolate(_)));
        assert_eq!(v[3], Verdict::Snap(Refusal::SharpChange));
        assert!(matches!(v[4], Verdict::Extrapolate(_)), "{:?}", v[4]);
        assert!(matches!(v[5], Verdict::Extrapolate(_)));
    }

    #[test]
    fn small_accelerations_are_not_sharp() {
        // 0, 1, 3, 6 per tick: a change of 1, 2, 3.
        let v = run(&[(0, 0), (1, 1), (4, 4), (10, 10)]);
        assert!(
            v[1..].iter().all(|x| matches!(x, Verdict::Extrapolate(_))),
            "{v:?}"
        );
    }

    #[test]
    fn a_teleport_snaps_and_forgets_the_velocity() {
        let v = run(&[(0, 10), (2, 12), (2, 400), (4, 402), (6, 404)]);
        assert_eq!(v[2], Verdict::Snap(Refusal::Teleport));
        // After a teleport there is no velocity to compare with, so the next tick is accepted.
        assert!(matches!(v[3], Verdict::Extrapolate(_)), "{:?}", v[3]);
        assert!(matches!(v[4], Verdict::Extrapolate(_)));
    }

    #[test]
    fn a_camera_jump_is_a_teleport() {
        let v = run(&[(0, 10), (2, 12), (200, 14)]);
        assert_eq!(v[2], Verdict::Snap(Refusal::Teleport));
    }

    #[test]
    fn a_scene_change_snaps() {
        let mut t = Tracker::new();
        assert!(matches!(t.tick(facts(0, 0, 0)), Verdict::Snap(_)));
        assert!(matches!(t.tick(facts(1, 1, 1)), Verdict::Extrapolate(_)));
        let mut f = facts(2, 2, 2);
        f.scene = 8;
        assert_eq!(t.tick(f), Verdict::Snap(Refusal::SceneCut));
        assert!(matches!(
            t.tick(facts(3, 3, 3)),
            Verdict::Snap(Refusal::SceneCut)
        ));
        assert!(matches!(t.tick(facts(4, 4, 4)), Verdict::Extrapolate(_)));
    }

    #[test]
    fn a_gap_in_the_frame_count_snaps() {
        let mut t = Tracker::new();
        let _ = t.tick(facts(0, 0, 0));
        let _ = t.tick(facts(1, 1, 1));
        assert_eq!(
            t.tick(facts(5, 2, 2)),
            Verdict::Snap(Refusal::NotConsecutive)
        );
        assert_eq!(
            t.tick(facts(5, 2, 2)),
            Verdict::Snap(Refusal::NotConsecutive)
        );
    }

    #[test]
    fn coordinates_that_wrap_are_small_steps() {
        let v = run(&[(0xFFFE, 0xFFFF), (0, 1), (2, 3)]);
        assert!(matches!(v[1], Verdict::Extrapolate(_)), "{:?}", v[1]);
        assert!(matches!(v[2], Verdict::Extrapolate(_)));
    }

    #[test]
    fn objects_are_counted_not_vetoed() {
        let mut t = Tracker::new();
        let mk = |i: u64, a: u16, b: u16| TickFacts {
            index: i,
            scene: 1,
            camera: (0, 0),
            player: None,
            objects: vec![Some((a, 0)), Some((b, 0)), None],
        };
        let _ = t.tick(mk(0, 0, 0));
        let _ = t.tick(mk(1, 1, 0));
        // Object 0 keeps going, object 1 teleports.
        let Verdict::Extrapolate(m) = t.tick(mk(2, 2, 500)) else {
            panic!("objects must not veto the frame");
        };
        assert_eq!((m.objects_moving, m.objects_snapped), (1, 1));
        // Object 0 reverses hard.
        let Verdict::Extrapolate(m) = t.tick(mk(3, 2u16.wrapping_sub(6), 500)) else {
            panic!("objects must not veto the frame");
        };
        assert_eq!(m.objects_snapped, 1);
    }

    /// Every sequence of ticks, however wild, terminates with a verdict per tick.
    #[test]
    fn wild_inputs_terminate() {
        let mut t = Tracker::new();
        for i in 0..2000u64 {
            let x = (i.wrapping_mul(2_654_435_761) >> 7) as u16;
            let _ = t.tick(TickFacts {
                index: i / 3,
                scene: i % 5,
                camera: (x, x.rotate_left(3)),
                player: (i % 4 != 0).then_some((x, x)),
                objects: vec![Some((x, x)); (i % 7) as usize],
            });
        }
    }
}
