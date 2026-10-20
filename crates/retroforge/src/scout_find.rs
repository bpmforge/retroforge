//! Ticket W33-07: the in-app flow of "Find scout settings for this game", the
//! core-thread half.
//!
//! The finder ([`rf_enhance::scout_finder::ScoutFinder`]) is an observer: each
//! live frame it is fed a copy of the work RAM, the pad and the scroll
//! registers. Once it has a camera, a player word and a jump to name a Y by,
//! and the player stands still (a snapshot of a player in mid-air or mid-death
//! would start every trial from a bad place), the candidates are tried on a
//! second machine ([`crate::scout_discovery`], on a thread of its own: the
//! live core never waits for it) and the answer goes back to the UI thread,
//! which offers to save it to the player's own profile.

use std::sync::mpsc::{channel, Receiver, TryRecvError};

use rf_core_api::{CoreEvent, FrameBundle};
use rf_enhance::canvas::{CanvasModel, LayerScroll, LAYERS};
use rf_enhance::scene_identity::SceneId;
use rf_enhance::scene_tracker::SETTLING;
use rf_enhance::scout_finder::{picture_hash, Frame, Pad, ScoutFinder, Skeleton, Vert};
use rf_input::{NesButton, SnesButton};

use crate::core_thread::ScoutFindProgress;
use crate::scene_key::SceneKeySpec;
use crate::scout_discovery::{discover, Context, Vertical};
use crate::stepper::{EmuStepper, MachineSnapshot};

/// Work RAM the SNES finder looks through (as the camera finder does).
const SEARCH_SNES: usize = 0x2000;
/// The NES's 2 KiB and the cartridge's 8 KiB RAM window.
const SEARCH_NES: usize = 0x0800 + 0x2000;
/// How often the finder's verdict is looked at.
const LOOK_EVERY: u64 = 30;
/// Frames the pad must have been idle before the trials start.
const STAND_FRAMES: u32 = 20;

/// What a frame of the live game hands the finder.
pub struct Fed<'a> {
    pub stepper: &'a EmuStepper,
    pub bundle: &'a FrameBundle,
    /// Port 0's pad word.
    pub pad: u16,
    pub scene: SceneId,
    pub rom: &'a [u8],
    pub canvases: &'a CanvasModel,
    pub size: (u16, u16),
    pub scene_key: Option<&'a SceneKeySpec>,
}

/// What the finder reports.
pub enum Out {
    Progress(ScoutFindProgress),
    Done(Result<Box<Skeleton>, String>),
}

type TrialResult = Result<Box<Skeleton>, String>;

/// The finder and the trial thread, for one game.
pub struct ScoutFind {
    finder: ScoutFinder,
    snes: bool,
    idle: u32,
    trial: Option<Receiver<TrialResult>>,
    last: ScoutFindProgress,
    /// Ticket W35-02: the machine, the live model and the scene of the first
    /// moment the player stood still in a room that scrolls vertically (the Y
    /// recipe can only be tried there).
    vertical: Option<(MachineSnapshot, CanvasModel, SceneId)>,
}

/// The SNES scroll registers the finder is fed: per background (BG1, BG2) the low bytes of its
/// x and y registers, EMPTY for a layer that is not the one the player scrolls. W35-04: a layer
/// that is switched off keeps its registers, and one that is only on the sub screen (a colour-math
/// overlay: A Link to the Past's BG1 is a half-speed rain layer that way) scrolls at its own pace;
/// the camera finder takes the first layer that moves, so it named the overlay's word as the
/// camera and no player step ever matched it. A sub-screen layer counts only when no layer is on
/// the main screen.
#[must_use]
pub fn finder_snes_scroll(stepper: &EmuStepper) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
    let bgs = stepper.bg_layer_views()?;
    let sub = stepper.sub_screen_bg_mask().unwrap_or(0);
    let any_main = bgs.iter().any(|b| b.enabled);
    Some(
        (0..2)
            .map(|i| {
                if bgs[i].enabled || (!any_main && sub & (1 << i) != 0) {
                    (
                        vec![(bgs[i].hofs & 0xFF) as u8],
                        vec![(bgs[i].vofs & 0xFF) as u8],
                    )
                } else {
                    (Vec::new(), Vec::new())
                }
            })
            .collect(),
    )
}

fn no_scrolls(_: &EmuStepper) -> Option<[Option<LayerScroll>; LAYERS]> {
    None
}

impl ScoutFind {
    #[must_use]
    pub fn new(snes: bool) -> Self {
        Self {
            finder: ScoutFinder::new(
                if snes { SEARCH_SNES } else { SEARCH_NES },
                if snes { 2 } else { 1 },
            ),
            snes,
            idle: 0,
            trial: None,
            last: ScoutFindProgress::default(),
            vertical: None,
        }
    }

    /// Feed one live frame; may start the trials or report their result.
    pub fn frame(&mut self, f: &Fed<'_>) -> Vec<Out> {
        if let Some(rx) = &self.trial {
            return match rx.try_recv() {
                Ok(result) => {
                    self.trial = None;
                    vec![Out::Done(result)]
                }
                Err(TryRecvError::Empty) => Vec::new(),
                Err(TryRecvError::Disconnected) => {
                    self.trial = None;
                    vec![Out::Done(
                        Err("the trials stopped without an answer".into()),
                    )]
                }
            };
        }
        let start_bit = if self.snes {
            SnesButton::Start.bit()
        } else {
            NesButton::Start.bit()
        };
        let (left, right, up, down, faces): (u8, u8, u8, u8, &[u8]) = if self.snes {
            (
                SnesButton::Left.bit(),
                SnesButton::Right.bit(),
                SnesButton::Up.bit(),
                SnesButton::Down.bit(),
                &[
                    SnesButton::B.bit(),
                    SnesButton::A.bit(),
                    SnesButton::X.bit(),
                    SnesButton::Y.bit(),
                ],
            )
        } else {
            (
                NesButton::Left.bit(),
                NesButton::Right.bit(),
                NesButton::Up.bit(),
                NesButton::Down.bit(),
                &[NesButton::A.bit(), NesButton::B.bit()],
            )
        };
        let held = |bit: u8| f.pad & (1 << bit) != 0;
        let pad = if held(right) {
            Pad::Right
        } else if held(left) {
            Pad::Left
        } else {
            Pad::Idle
        };
        let vert = if held(down) {
            Vert::Down
        } else if held(up) {
            Vert::Up
        } else {
            Vert::Idle
        };
        let jump = faces
            .iter()
            .enumerate()
            .filter(|(_, &b)| held(b))
            .fold(0u8, |m, (i, _)| m | (1 << i));
        self.idle = if pad == Pad::Idle && vert == Vert::Idle && jump == 0 {
            self.idle.saturating_add(1)
        } else {
            0
        };
        let scene = (f.scene != SETTLING).then_some(f.scene.0);
        let nes_ram: Vec<u8>;
        let ram: &[u8] = match f.stepper.snes_work_ram_head(SEARCH_SNES) {
            Some(r) => r,
            None => {
                nes_ram = [&f.stepper.wram_snapshot()[..], &f.stepper.prg_ram()[..]].concat();
                &nes_ram
            }
        };
        let scroll: Vec<(Vec<u8>, Vec<u8>)> = match finder_snes_scroll(f.stepper) {
            // SNES: BG1 then BG2, the low bytes of the registers.
            Some(layers) => layers,
            // NES: every $2005/$2006 write this frame.
            None => {
                let (mut xs, mut ys) = (Vec::new(), Vec::new());
                for e in &f.bundle.events {
                    if let CoreEvent::ScrollWrite { x, y, .. } = e {
                        xs.push((x & 0xFF) as u8);
                        ys.push((y & 0xFF) as u8);
                    }
                }
                vec![(xs, ys)]
            }
        };
        let layers: Vec<(&[u8], &[u8])> = scroll
            .iter()
            .map(|(x, y)| (x.as_slice(), y.as_slice()))
            .collect();
        if self.snes
            && self.vertical.is_none()
            && self.idle >= STAND_FRAMES
            && scene.is_some()
            && f.canvases.scene_scrolls_vertically() == Some(true)
        {
            // Read only; once per session.
            if let Ok(snapshot) = f.stepper.snapshot() {
                self.vertical = Some((snapshot, f.canvases.fork_for_scout(), f.scene));
            }
        }
        self.finder.observe(&Frame {
            ram,
            pad,
            vert,
            jump,
            scroll: &layers,
            scene,
            start: f.pad & (1 << start_bit) != 0,
            shown: picture_hash(&f.bundle.video),
        });
        if !self.finder.frames().is_multiple_of(LOOK_EVERY) {
            return Vec::new();
        }
        let obs = self.finder.observed();
        let mut out = Vec::new();
        let progress = ScoutFindProgress {
            camera: obs.camera.is_some(),
            player: obs.players.first().is_some_and(|p| p.hi.is_some()),
            y: !obs.ys.is_empty(),
            gate: !obs.gates.is_empty(),
            testing: false,
            moving: self.finder.moving(),
        };
        if obs.ready() && self.idle >= STAND_FRAMES && scene.is_some() {
            // Start the trials: one snapshot of the live machine, read only.
            match f.stepper.snapshot() {
                Ok(snapshot) => {
                    let (tx, rx) = channel();
                    let rom = f.rom.to_vec();
                    let seed = f.canvases.fork_for_scout();
                    let (scene, size, snes) = (f.scene, f.size, self.snes);
                    let scene_key = f.scene_key.cloned();
                    let vertical = self.vertical.take();
                    std::thread::spawn(move || {
                        let ctx = Context {
                            rom: &rom,
                            live: &seed,
                            scene,
                            size,
                            snes,
                            scrolls: if snes {
                                crate::core_thread::snes_layer_scrolls
                            } else {
                                no_scrolls
                            },
                            scene_key,
                            vertical: vertical.as_ref().map(|(snapshot, live, scene)| Vertical {
                                snapshot: snapshot.clone(),
                                live,
                                scene: *scene,
                            }),
                        };
                        let result = discover(snapshot, ctx, &obs)
                            .map(|(skeleton, _)| Box::new(skeleton))
                            .map_err(|e| e.to_string());
                        let _ = tx.send(result);
                    });
                    self.trial = Some(rx);
                    out.push(Out::Progress(ScoutFindProgress {
                        testing: true,
                        ..progress
                    }));
                    self.last = ScoutFindProgress {
                        testing: true,
                        ..progress
                    };
                    return out;
                }
                Err(e) => {
                    return vec![Out::Done(Err(format!("could not take a snapshot: {e:?}")))];
                }
            }
        }
        if progress != self.last {
            self.last = progress;
            out.push(Out::Progress(progress));
        }
        out
    }
}
