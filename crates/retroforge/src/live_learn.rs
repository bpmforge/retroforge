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

use std::sync::Arc;

use rf_enhance::canvas::CanvasModel;
use rf_enhance::heights::{
    cluster, Dirs, Entity, FrameInput, FrameObs, HeightGrid, HeightLearner, Orientation,
    OrientationGuess, OrientationSource, PlayerTracker, SpriteBox, Stats, TileGrid,
};
use rf_enhance::scene_tracker::SceneTracker;

use crate::hud_watch::{self, HudInfo, HudWatch};
use crate::level_heights::{grid_phase, LayerFormat, LevelHeights, LevelTracker};
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

/// Ticket W34-08: what the Diorama needs to pull the picture apart into
/// planes: which layer drew each pixel, how fast each layer scrolled, and
/// which part of the picture is HUD.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LayerInfo {
    /// Per pixel of the frame, the background layer (0..=3) that drew it,
    /// [`hud_watch::LAYER_BACKDROP`] for the backdrop colour and
    /// [`hud_watch::LAYER_SPRITE`] for a sprite. Empty when the frame is not
    /// the bundle's own size.
    pub ids: Vec<u8>,
    /// Scroll ratio per layer against the fastest (1.0 is the playfield).
    pub ratios: [Option<f32>; 4],
    pub hud: HudInfo,
}

/// The layer map of a frame's pixels (`LayerInfo::ids`).
///
/// A pixel the main screen leaves to the backdrop can still show a layer: one
/// the game draws on the SUB screen and blends in with colour math (Super
/// Mario World's hills are such a layer; the main-screen metadata calls every
/// one of those dots backdrop). Where the sub screen's own pixel at that dot
/// belongs to a background layer, the dot is that layer's.
#[must_use]
pub fn layer_ids(bundle: &rf_core_api::FrameBundle) -> Vec<u8> {
    use rf_core_api::{ColorMathOp, PixelLayer};
    let width = usize::from(bundle.width).max(1);
    bundle
        .video
        .iter()
        .enumerate()
        .map(|(i, p)| match p.layer {
            PixelLayer::Background(n) => n.min(3),
            PixelLayer::Sprite => hud_watch::LAYER_SPRITE,
            PixelLayer::Backdrop => bundle
                .sub
                .get(i / width)
                .and_then(Option::as_ref)
                .and_then(|line| line.pixels.get(i % width))
                .filter(|sp| !sp.fixed && sp.op != ColorMathOp::None)
                .and_then(|sp| match sp.layer {
                    PixelLayer::Background(n) => Some(n.min(3)),
                    _ => None,
                })
                .unwrap_or(hud_watch::LAYER_BACKDROP),
        })
        .collect()
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
    /// Ticket W34-07: the scene's heights over its whole canvas as they stand
    /// (shared, a pointer per frame; the shell keeps the latest).
    pub level: Option<Arc<LevelHeights>>,
    /// Ticket W34-08: the layers of the picture and its HUD.
    pub layers: LayerInfo,
}

/// The learner and everything that feeds it, for one session.
pub struct LiveHeights {
    scene: SceneTracker,
    tracker: PlayerTracker,
    learner: HeightLearner,
    entities: Vec<Entity>,
    snes_cam: (i64, i64, Option<(u16, u16)>),
    snes: bool,
    /// Guard refusals already logged.
    guard_logged: u32,
    /// Ticket W34-07: what the canvases add (whole-level heights) and what
    /// is kept between sessions.
    level: LevelTracker,
    restored: bool,
    /// The profile's `[camera].mode`.
    hint: Option<Orientation>,
    /// The call an earlier session settled on, used until this one's own
    /// evidence is sure (a profile's hint wins over it).
    prior: Option<Orientation>,
    /// Ticket W34-08: which part of the picture is HUD.
    hud: HudWatch,
}

impl LiveHeights {
    #[must_use]
    pub fn new(snes: bool, rom_sha256: &str) -> Self {
        Self {
            scene: SceneTracker::new(),
            tracker: PlayerTracker::new(),
            learner: HeightLearner::new(),
            entities: Vec::new(),
            snes_cam: (0, 0, None),
            snes,
            guard_logged: 0,
            level: LevelTracker::new(rom_sha256.to_string()),
            restored: false,
            hint: None,
            prior: None,
            hud: HudWatch::new(),
        }
    }

    /// The profile's `[camera].mode`, when it says (settles the orientation).
    pub fn set_hint(&mut self, hint: Option<Orientation>) {
        self.hint = hint;
        self.tracker.set_mode_hint(hint);
    }

    /// Ticket W34-07: start from what an earlier session left for this ROM
    /// (its tile table and its call on the camera). Once, before the first
    /// frame.
    fn restore(&mut self, canvases: &mut CanvasModel) {
        self.restored = true;
        if let Some(t) = self.level.persist().load_table(canvases) {
            log::info!("height learner: restored {} tiles from the cache", t.len());
            self.learner = HeightLearner::with_table(t);
        }
        self.prior = self.level.persist().load_orientation(canvases);
    }

    /// Ticket W34-07: queue what is worth keeping; the canvas model writes it
    /// with the scenes. Called as the core thread stops.
    pub fn finish(&mut self, canvases: &mut CanvasModel) {
        let o = self.tracker.observed_orientation();
        let kept = if Self::worth_keeping(o) {
            o.orientation
        } else {
            Orientation::Unknown
        };
        self.level.finish(canvases);
        self.level.persist().save_session(
            canvases,
            &self.learner,
            kept,
            self.learner.frames(),
            true,
        );
    }

    /// A call on the camera that came from watching the game, and is sure.
    fn worth_keeping(o: OrientationGuess) -> bool {
        o.confidence >= 2
            && matches!(
                o.source,
                OrientationSource::Gravity | OrientationSource::ConstantSpeed
            )
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
        canvases: &mut CanvasModel,
    ) -> HeightsFrame {
        if !self.restored {
            self.restore(canvases);
        }
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
        // A remembered call stands in for the picture's orientation until the
        // game's own evidence is sure. It only decides how the heights are
        // DRAWN: the learner is fed what was observed (or the profile's
        // hint), so a menu is never learned from as if it were the level.
        let own = self.tracker.orientation();
        if self.tracker.observed_orientation().confidence >= 2 {
            self.prior = None;
        }
        let orientation = match (self.hint, self.prior) {
            (None, Some(p)) => OrientationGuess {
                orientation: p,
                confidence: 3,
                source: OrientationSource::Hint,
            },
            _ => own,
        };
        self.learner.observe(&FrameObs {
            grids,
            sprites,
            entities: &self.entities,
            player: self.tracker.player(),
            pad,
            orientation: own.orientation,
            hidden: None,
        });
        match (self.learner.guard_trip(), self.guard_logged) {
            (Some(t), n) if n != self.learner.guard_trips() => {
                self.guard_logged = self.learner.guard_trips();
                log::warn!(
                    "height learner: refused a table raising {} of {} cells of layer {} \
                     (over {}%); drawing flat and forgetting those tiles",
                    t.raised,
                    t.cells,
                    t.layer,
                    rf_enhance::heights::learner::MAX_RAISED_PCT
                );
            }
            _ => {}
        }
        let level = self.level_pass(stepper, grids, canvases);
        let observed = self.tracker.observed_orientation();
        self.level.persist().save_session(
            canvases,
            &self.learner,
            if Self::worth_keeping(observed) {
                observed.orientation
            } else {
                Orientation::Unknown
            },
            self.learner.frames(),
            false,
        );
        let shown = self.tracker.player().or_else(|| self.tracker.tracked());
        let foreground = self.learner.foreground_index(grids);
        let ids = layer_ids(bundle);
        let visible = hud_watch::visible_rows(&ids, usize::from(bundle.width));
        let hud = self
            .hud
            .observe(grids, foreground, &visible, u32::from(bundle.height));
        HeightsFrame {
            layers: LayerInfo {
                ids,
                ratios: self.learner.layer_ratios(),
                hud,
            },
            level,
            grids: self.learner.grids_for(grids),
            foreground,
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

impl LiveHeights {
    /// Ticket W34-07: keep the scene's whole-level heights current. The
    /// playfield's block grid is placed on the layer's canvas by where the
    /// frame sits there; a console or layer that cannot be re-tiled
    /// (`LayerFormat::for_mode`) gets only the scene bookkeeping.
    fn level_pass(
        &mut self,
        stepper: &EmuStepper,
        grids: &[TileGrid],
        canvases: &mut CanvasModel,
    ) -> Option<Arc<LevelHeights>> {
        let frames = canvases.frames()?;
        let fg = self.learner.foreground_index(grids);
        let fmt = fg.and_then(|i| {
            let g = grids.get(i).filter(|g| g.exact && !g.hud)?;
            let mode = stepper.snes_bg_mode()?;
            let fmt = LayerFormat::for_mode(mode, g.layer, g.cell_px != 8)?;
            let (ox, oy) = frames.origin_at(usize::from(g.layer), usize::from(g.rows_px.0))?;
            Some((fmt, grid_phase((ox, oy), g.offset, g.rows_px.0)))
        });
        let g = fg.and_then(|i| grids.get(i));
        let origin =
            g.and_then(|g| frames.origin_at(usize::from(g.layer), usize::from(g.rows_px.0)));
        self.level.observe(
            self.learner.frames(),
            frames.scene,
            fmt,
            g.zip(origin),
            canvases,
            &self.learner,
        );
        let level = self.level.current();
        // Proof that the canvas and the capture name tiles alike (counted in
        // `level_heights::telemetry`), a few times a second.
        if fmt.is_some() && self.learner.frames().is_multiple_of(15) {
            if let (Some(g), Some(o)) = (g, origin) {
                self.level.audit(g, o);
            }
        }
        level
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
    fn a_session_starts_from_the_table_and_call_the_last_one_kept() {
        use rf_enhance::heights::{Class, Entry, Learned, TileHeightTable, TileKey};
        let root =
            std::env::temp_dir().join(format!("rf_live_learn_restore_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let open = || {
            CanvasModel::with_cache(
                "rom-w34-07".to_string(),
                Some(rf_cache::Cache::open(&root, 1 << 20).unwrap()),
            )
        };
        let key = TileKey {
            pattern: 7,
            palette: 9,
            layer: 0,
            cell_px: 8,
        };
        let mut table = TileHeightTable::new();
        table.insert(
            key,
            Entry {
                learned: Learned {
                    class: Class::Wall,
                    height_eighths: 8,
                    confidence: 3,
                },
                ..Entry::default()
            },
        );
        // Session one: a learner with a firm call and a table, finished.
        let mut canvases = open();
        let mut first = LiveHeights::new(true, "rom-w34-07");
        first.learner = HeightLearner::with_table(table.clone());
        first.level.persist().save_session(
            &mut canvases,
            &first.learner,
            Orientation::SideView,
            1,
            true,
        );
        canvases.flush();
        drop(canvases);
        // Session two restores both, once.
        let mut canvases = open();
        let mut second = LiveHeights::new(true, "rom-w34-07");
        assert!(second.prior.is_none() && second.learner.table().is_empty());
        second.restore(&mut canvases);
        assert_eq!(second.learner.table(), &table);
        assert_eq!(second.prior, Some(Orientation::SideView));
        // A profile's own word is kept apart from the remembered one.
        second.set_hint(Some(Orientation::TopDown));
        assert_eq!(second.hint, Some(Orientation::TopDown));
        assert_eq!(second.prior, Some(Orientation::SideView));
        // Only a call watched from play and sure of itself is worth keeping.
        let call = |orientation, confidence, source| OrientationGuess {
            orientation,
            confidence,
            source,
        };
        assert!(LiveHeights::worth_keeping(call(
            Orientation::SideView,
            2,
            OrientationSource::Gravity
        )));
        assert!(!LiveHeights::worth_keeping(call(
            Orientation::SideView,
            1,
            OrientationSource::Gravity
        )));
        assert!(!LiveHeights::worth_keeping(call(
            Orientation::SideView,
            3,
            OrientationSource::Hint
        )));
        assert!(!LiveHeights::worth_keeping(call(
            Orientation::SideView,
            3,
            OrientationSource::ScrollAxis
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_pad_mask_reads_the_right_bits_for_each_console() {
        use rf_input::{NesButton, SnesButton};
        let snes = LiveHeights::new(true, "");
        let nes = LiveHeights::new(false, "");
        let s = 1u16 << SnesButton::Right.bit();
        let n = 1u16 << NesButton::Right.bit();
        assert!(snes.dirs(s).right && !snes.dirs(s).left);
        assert!(nes.dirs(n).right && !nes.dirs(n).up);
        assert_eq!(snes.dirs(0), Dirs::default());
    }
}

#[cfg(test)]
mod layer_tests {
    use super::*;
    use rf_core_api::{ColorMathOp, FrameBundle, PixelLayer, PpuPixel, SubLine, SubPixel};

    fn px(layer: PixelLayer) -> PpuPixel {
        PpuPixel {
            palette_index: 0,
            layer,
            sprite_id: None,
            priority: 0,
        }
    }

    #[test]
    fn a_sub_screen_layer_blended_over_the_backdrop_is_that_layers_pixel() {
        let video = vec![
            px(PixelLayer::Background(0)),
            px(PixelLayer::Backdrop),
            px(PixelLayer::Backdrop),
            px(PixelLayer::Backdrop),
            px(PixelLayer::Sprite),
        ];
        let mut b = FrameBundle::new(1, 5, 1, video, Vec::new());
        let sub = |layer, op, fixed| SubPixel {
            layer,
            op,
            fixed,
            ..SubPixel::default()
        };
        b.sub = vec![Some(SubLine {
            pixels: vec![
                sub(PixelLayer::Background(1), ColorMathOp::Add, false),
                // Backdrop dot over a BG2 sub-screen pixel with math: BG2's.
                sub(PixelLayer::Background(1), ColorMathOp::Add, false),
                // Over the fixed colour: the sky.
                sub(PixelLayer::Background(1), ColorMathOp::Add, true),
                // No colour math here: the sub screen is not shown.
                sub(PixelLayer::Background(1), ColorMathOp::None, false),
                sub(PixelLayer::Backdrop, ColorMathOp::None, false),
            ],
            fixed_color: 0,
        })];
        assert_eq!(
            layer_ids(&b),
            [
                0,
                1,
                hud_watch::LAYER_BACKDROP,
                hud_watch::LAYER_BACKDROP,
                hud_watch::LAYER_SPRITE
            ]
        );
        // A frame the builder did not capture sub screens for: all backdrop.
        b.sub.clear();
        assert_eq!(layer_ids(&b)[1], hud_watch::LAYER_BACKDROP);
    }
}
