//! The automatic heights, running (ticket W34-04; `docs/design/HD2D_PLAN.md`
//! section 2.3).
//!
//! The core thread owns one [`LiveHeights`] while Diorama is on outside
//! Accuracy Mode (`CoreCommand::SetTileGridCapture`). Each frame it is fed
//! what the capture already produced -- the tile grids, the sprite boxes,
//! the pad -- plus the camera position, and it hands back a
//! [`HeightsFrame`]: the learned height of every cell of every grid, which
//! grid is the playfield, the orientation call and the counts the readout
//! shows. It only reads (law 4): nothing here reaches the machine.
//!
//! The camera is the NES scene tracker's world position, or on the SNES the
//! first background's scroll registers unwrapped across their 10-bit wrap,
//! exactly as the W34-03 oracle harness feeds the tracker.

use rf_enhance::heights::{
    cluster, Dirs, Entity, FrameInput, FrameObs, HeightGrid, HeightLearner, Orientation,
    OrientationGuess, PlayerTracker, SpriteBox, Stats, TileGrid,
};
use rf_enhance::scene_tracker::SceneTracker;

use crate::stepper::EmuStepper;

/// Which way the camera looks, from a profile's `[camera].mode`.
#[must_use]
pub fn orientation_of_mode(mode: &str) -> Option<Orientation> {
    match mode {
        "side_scroller" => Some(Orientation::SideView),
        "top_down_rooms" => Some(Orientation::TopDown),
        _ => None,
    }
}

/// The tracked player as the renderer wants it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerView {
    /// Screen pixels, centre of the entity's box.
    pub centre: (i32, i32),
    /// On screen this frame (false: held where it was last seen).
    pub visible: bool,
}

/// What the learner knew after one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct HeightsFrame {
    /// One per tile grid of the frame, same order, read back from the table.
    pub grids: Vec<HeightGrid>,
    /// The playfield grid (index into the frame's tile grids).
    pub foreground: Option<usize>,
    pub orientation: OrientationGuess,
    pub stats: Stats,
    /// Scroll ratio of the slowest backdrop plane, when there is one.
    pub backdrop_ratio: Option<f32>,
    pub player: Option<PlayerView>,
    /// Frames the learner has watched.
    pub frames: u32,
}

/// The learner and everything that feeds it, for one session.
pub struct LiveHeights {
    scene: SceneTracker,
    tracker: PlayerTracker,
    learner: HeightLearner,
    entities: Vec<Entity>,
    snes_cam: (i64, i64, Option<(u16, u16)>),
    snes: bool,
}

impl LiveHeights {
    #[must_use]
    pub fn new(snes: bool) -> Self {
        Self {
            scene: SceneTracker::new(),
            tracker: PlayerTracker::new(),
            learner: HeightLearner::new(),
            entities: Vec::new(),
            snes_cam: (0, 0, None),
            snes,
        }
    }

    /// The profile's `[camera].mode`, when it says (settles the orientation).
    pub fn set_hint(&mut self, hint: Option<Orientation>) {
        self.tracker.set_mode_hint(hint);
    }

    /// The camera's world position (any origin), when known.
    fn camera(&mut self, stepper: &EmuStepper) -> Option<(i64, i64)> {
        if !self.snes {
            return self.scene.last_world_position();
        }
        let v = stepper.bg_layer_views()?[0];
        let (h, vv) = (v.hofs & 0x3FF, v.vofs & 0x3FF);
        let unwrap = |new: u16, old: u16| {
            let d = (i64::from(new) - i64::from(old)).rem_euclid(1024);
            if d >= 512 {
                d - 1024
            } else {
                d
            }
        };
        if let Some((oh, ov)) = self.snes_cam.2 {
            self.snes_cam.0 += unwrap(h, oh);
            self.snes_cam.1 += unwrap(vv, ov);
        }
        self.snes_cam.2 = Some((h, vv));
        Some((self.snes_cam.0, self.snes_cam.1))
    }

    /// The pad's four directions from the port-1 mask.
    #[must_use]
    pub fn dirs(&self, mask: u16) -> Dirs {
        use rf_input::{NesButton, SnesButton};
        let has = |snes: SnesButton, nes: NesButton| {
            mask & (1 << if self.snes { snes.bit() } else { nes.bit() }) != 0
        };
        Dirs {
            left: has(SnesButton::Left, NesButton::Left),
            right: has(SnesButton::Right, NesButton::Right),
            up: has(SnesButton::Up, NesButton::Up),
            down: has(SnesButton::Down, NesButton::Down),
        }
    }

    /// Fold one frame in and read the heights back for its grids.
    pub fn frame(
        &mut self,
        stepper: &EmuStepper,
        bundle: &rf_core_api::FrameBundle,
        pad_mask: u16,
        grids: &[TileGrid],
        sprites: &[SpriteBox],
    ) -> HeightsFrame {
        if !self.snes {
            self.scene.observe_frame(
                &bundle.video,
                bundle.width,
                bundle.height,
                &bundle.events,
                &[],
            );
        }
        let camera = self.camera(stepper);
        let pad = self.dirs(pad_mask);
        cluster(sprites, &mut self.entities);
        self.tracker.observe(&FrameInput {
            sprites,
            pad,
            camera,
            hint_x: None,
            hint_y: None,
        });
        let orientation = self.tracker.orientation();
        self.learner.observe(&FrameObs {
            grids,
            sprites,
            entities: &self.entities,
            player: self.tracker.player(),
            pad,
            orientation: orientation.orientation,
            hidden: None,
        });
        let shown = self.tracker.player().or_else(|| self.tracker.tracked());
        HeightsFrame {
            grids: grids.iter().map(|g| self.learner.grid_for(g)).collect(),
            foreground: self.learner.foreground_index(grids),
            orientation,
            stats: self.learner.stats(),
            backdrop_ratio: self.learner.backdrop_ratio(),
            player: shown.map(|p| PlayerView {
                centre: (
                    (p.entity.x0 + p.entity.x1) / 2,
                    (p.entity.y0 + p.entity.y1) / 2,
                ),
                visible: p.visible,
            }),
            frames: self.learner.frames(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_camera_mode_settles_the_orientation() {
        assert_eq!(
            orientation_of_mode("side_scroller"),
            Some(Orientation::SideView)
        );
        assert_eq!(
            orientation_of_mode("top_down_rooms"),
            Some(Orientation::TopDown)
        );
        assert_eq!(orientation_of_mode("default"), None);
    }

    #[test]
    fn the_pad_mask_reads_the_right_bits_for_each_console() {
        use rf_input::{NesButton, SnesButton};
        let snes = LiveHeights::new(true);
        let nes = LiveHeights::new(false);
        let s = 1u16 << SnesButton::Right.bit();
        let n = 1u16 << NesButton::Right.bit();
        assert!(snes.dirs(s).right && !snes.dirs(s).left);
        assert!(nes.dirs(n).right && !nes.dirs(n).up);
        assert_eq!(snes.dirs(0), Dirs::default());
    }
}
