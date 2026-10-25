//! Ticket W36-06b: draw the objects a recipe mod keeps alive in the widescreen margins.
//!
//! The live-enemies mod (W36-06) makes the game simulate enemies far outside the 256-px picture. The
//! game still draws with 8-bit sprite X, so it hides those enemies (or, rarely, wraps them onto the real
//! picture). This module makes them visible in the pad, **from the game's own sprite data and without
//! touching the real machine**:
//!
//! 1. Each real frame the shell keeps the machine's state from just BEFORE the frame (a ring of three).
//! 2. When an object sits outside the picture (the recipe's object table says where), a private
//!    "redraw" machine is loaded with the state from two frames back, its camera is moved by one page
//!    toward the object (both edges, from recipe RAM meanings), and it is run three frames with the
//!    real inputs. Why three: the picture of frame N is drawn from the OAM that was copied in the
//!    vblank before it, from a buffer built by the game logic two frames earlier (observed:
//!    `w36_06b_oam_observation::which_frames_oam_does_the_picture_come_from`), so the camera has to be
//!    moved before THAT logic runs. The sprites the PPU drew in the third frame are the picture the game
//!    would have shown with the object on screen.
//! 3. [`rf_enhance::margin_draw::keep_near`] keeps only the tiles at the objects' expected spots (not the
//!    player's), [`rf_enhance::margin_draw::rasterise`] paints them from the redraw machine's own
//!    pattern bytes and palette into an indexed strip, and the UI thread paints the strip over the pad.
//! 4. When the game also drew the object, wrapped, on the real picture (observed for a minority of
//!    frames), the OAM entries are the same bytes in both machines; the dots they won are returned so
//!    the compositor can give them back to the background.
//!
//! The real core, its sprite evaluation, sprite-0 hit and overflow are never involved (law 4/6). Only
//! machines built from the same (patched) image run, so a recipe mod is the same code in all of them.
//!
//! **A hook host that holds `write_memory` changes the machine in ways the redraw machine (and
//! run-ahead's shadow) cannot reproduce, so both step aside** ([`blocked_by_hooks`]).

use std::collections::VecDeque;

use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};
use rf_enhance::margin_draw::{self, Avoid, MarginDraw, Object, Tile};

use crate::stepper::{EmuStepper, MachineSnapshot};

/// Frames the redraw runs: the camera is moved before the first, the picture is the third.
const REPLAY_FRAMES: usize = 3;

/// Picture rows and width of a NES frame.
const HEIGHT: usize = 240;
const WIDTH: usize = 256;

/// The recipe fields the redraw reads, resolved to RAM addresses.
#[derive(Debug, Clone)]
struct Map {
    /// Number of object slots (the width of the flag row).
    slots: u32,
    flag: u32,
    x_page: u32,
    x_low: u32,
    y: u32,
    left_page: u32,
    left_x: u32,
    right_page: u32,
    right_x: u32,
    player_x: Option<u32>,
    player_y: Option<u32>,
}

impl Map {
    /// Read the object table out of `recipe`'s RAM meanings. `None` when the recipe does not describe
    /// one (the names are the recipe vocabulary documented in `docs/design/REBUILD_LAB.md`).
    fn from_recipe(recipe: &rf_rebuild::Recipe) -> Option<Self> {
        let row = |name: &str| recipe.ram.iter().find(|r| r.name == name);
        let addr = |name: &str| row(name).map(|r| r.addr);
        Some(Self {
            slots: u32::from(row("enemy_flag")?.width),
            flag: addr("enemy_flag")?,
            x_page: addr("enemy_x_page")?,
            x_low: addr("enemy_x")?,
            y: addr("enemy_y")?,
            left_page: addr("screen_left_page")?,
            left_x: addr("screen_left_x")?,
            right_page: addr("screen_right_page")?,
            right_x: addr("screen_right_x")?,
            player_x: addr("player_screen_x"),
            player_y: addr("player_y"),
        })
    }
}

/// Which pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

impl Side {
    /// Pages the camera moves by: the object is a whole page away from the picture.
    fn page_step(self) -> u8 {
        match self {
            Side::Left => 0xFF,
            Side::Right => 1,
        }
    }

    /// The same move as a signed page count.
    fn camera_pages(self) -> i32 {
        match self {
            Side::Left => -1,
            Side::Right => 1,
        }
    }

    fn bit(self) -> u8 {
        match self {
            Side::Left => 1,
            Side::Right => 2,
        }
    }
}

/// One object slot as the game's tables say it is.
#[derive(Debug, Clone, Copy)]
struct Slot {
    index: u32,
    /// x of the object's left edge minus the camera's.
    rel: i32,
    y: i32,
}

/// Active slots of the machine behind `peek`.
fn slots(map: &Map, peek: &dyn Fn(u32) -> u8) -> Vec<Slot> {
    let camera = i32::from(peek(map.left_page)) * 256 + i32::from(peek(map.left_x));
    (0..map.slots)
        .filter(|k| peek(map.flag + k) != 0)
        .map(|k| {
            let x = i32::from(peek(map.x_page + k)) * 256 + i32::from(peek(map.x_low + k));
            Slot {
                index: k,
                // The tables are 16 bits wide and wrap: take the difference as a signed 16-bit number.
                rel: i32::from((x - camera) as i16),
                y: i32::from(peek(map.y + k)),
            }
        })
        .collect()
}

/// A sink that discards the redraw machine's frames.
struct Discard;

impl CoreSink for Discard {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// A state from just before a real frame, and the input that frame ran with.
struct Pre {
    snapshot: MachineSnapshot,
    input: InputFrame,
}

/// What the compositor shows for a frame, as the machine that produced it saw it.
pub struct Shown<'a> {
    /// Frames the shown picture is ahead of the real machine (run-ahead; 0 for the real frame).
    pub ahead: u8,
    /// The OAM that drew the shown picture.
    pub oam: &'a [u8; 256],
    /// The OAM index that won each dot of the shown picture (`0xFF` none), when captured.
    pub ids: Option<&'a [u8]>,
    /// The background colour under each dot of the shown picture, when captured.
    pub bg_under: &'a [u8],
}

/// Per-session state of the margin redraw.
pub struct MarginDrawer {
    map: Map,
    rom: Vec<u8>,
    shadow: Option<EmuStepper>,
    ring: VecDeque<Pre>,
    /// Sides with an object outside the picture in each of the last frames (bit 0 left, 1 right).
    recent: [u8; REPLAY_FRAMES],
    /// The real machine's OAM at the end of the previous frame: what drew the current picture.
    last_oam: [u8; 256],
    /// Redraw frames run, for the cost readout.
    pub frames_replayed: u64,
}

/// Whether a loaded hook host makes the machine something a private copy cannot reproduce.
#[must_use]
pub fn blocked_by_hooks(real: &EmuStepper) -> bool {
    real.hook_host()
        .is_some_and(rf_plugin_sdk::HookHost::holds_write_memory)
}

impl MarginDrawer {
    /// `rom` is the image the real machine runs (patched when a mod is on). `None` when the recipe has no
    /// object table.
    #[must_use]
    pub fn new(rom: &[u8], recipe: &rf_rebuild::Recipe) -> Option<Self> {
        Some(Self {
            map: Map::from_recipe(recipe)?,
            rom: rom.to_vec(),
            shadow: None,
            ring: VecDeque::new(),
            recent: [0; REPLAY_FRAMES],
            last_oam: [0xF8; 256],
            frames_replayed: 0,
        })
    }

    /// Call before each real frame runs: keep the state it starts from.
    pub fn before_frame(&mut self, real: &EmuStepper, input: InputFrame) {
        match real.snapshot() {
            Ok(snapshot) => {
                if self.ring.len() == REPLAY_FRAMES {
                    self.ring.pop_front();
                }
                self.ring.push_back(Pre { snapshot, input });
            }
            Err(_) => self.ring.clear(),
        }
    }

    /// Forget the history (a state was loaded, the game was rewound or stepped): the ring no longer
    /// describes the frames leading to the machine.
    pub fn reset(&mut self) {
        self.ring.clear();
        self.recent = [0; REPLAY_FRAMES];
    }

    /// The redraw for the frame the real machine just ran (`shown.ahead == 0`) or for the picture
    /// `shown.ahead` frames after it (run-ahead; `held` is the input assumed for the frames not run yet).
    ///
    /// `None` when nothing needs drawing, or the ring does not describe the frames leading here.
    pub fn frame(
        &mut self,
        real: &EmuStepper,
        held: InputFrame,
        shown: &Shown<'_>,
    ) -> Option<Box<MarginDraw>> {
        let out = self.compute(real, held, shown);
        // Only now: `shown.oam` of a real frame is [`Self::oam_of_current_picture`], read before this call.
        self.last_oam = *real.oam();
        out
    }

    fn compute(
        &mut self,
        real: &EmuStepper,
        held: InputFrame,
        shown: &Shown<'_>,
    ) -> Option<Box<MarginDraw>> {
        let now = slots(&self.map, &|a| real.peek_bus(a));
        let mut sides = 0u8;
        for s in &now {
            if s.rel >= 240 {
                sides |= Side::Right.bit();
            }
            if s.rel < 16 {
                sides |= Side::Left.bit();
            }
        }
        self.recent.rotate_left(1);
        self.recent[REPLAY_FRAMES - 1] = sides;
        let wanted = self.recent.iter().fold(0, |a, b| a | b);
        if wanted == 0 || shown.ahead > 2 {
            return None;
        }
        // The ring must be the three frames ending with the one that just ran.
        let fc = real.frame_count();
        let contiguous = self.ring.len() == REPLAY_FRAMES
            && self
                .ring
                .iter()
                .enumerate()
                .all(|(i, p)| p.snapshot.frames() + 1 + (REPLAY_FRAMES - 1 - i) as u64 == fc);
        if !contiguous {
            return None;
        }
        let start = usize::from(shown.ahead);
        let inputs: [InputFrame; REPLAY_FRAMES] = std::array::from_fn(|i| {
            // Frame `start + i` of the ring is real up to the last entry; later ones are assumed.
            self.ring.get(start + i).map_or(held, |p| p.input)
        });
        let snapshot = self.ring.get(start)?.snapshot.clone();
        let mut left = vec![margin_draw::TRANSPARENT; WIDTH * HEIGHT];
        let mut right = left.clone();
        let mut ghost_idx: Vec<u8> = Vec::new();
        for side in [Side::Left, Side::Right] {
            if wanted & side.bit() == 0 {
                continue;
            }
            let Some(done) = self.redraw(real, &snapshot, inputs, side) else {
                continue;
            };
            match side {
                Side::Left => left = done.strip,
                Side::Right => right = done.strip,
            }
            for i in margin_draw::ghost_indices(shown.oam, &done.oam_before_picture, &done.kept) {
                if !ghost_idx.contains(&i) {
                    ghost_idx.push(i);
                }
            }
        }
        let ghosts = shown
            .ids
            .map(|ids| margin_draw::ghost_dots(ids, WIDTH, &ghost_idx, shown.bg_under))
            .unwrap_or_default();
        let md = MarginDraw {
            height: HEIGHT,
            left,
            right,
            ghosts,
        };
        (!md.is_empty()).then(|| Box::new(md))
    }

    /// The real machine's OAM at the end of the previous frame: what drew the picture now on screen.
    #[must_use]
    pub fn oam_of_current_picture(&self) -> [u8; 256] {
        self.last_oam
    }

    /// The sprites the redraw machine draws for the frame the real machine just ran when NOTHING is
    /// moved: the determinism check behind the whole approach (the replay must reproduce the real
    /// picture's sprites), and what the tests compare with the real core's own. Test support.
    #[doc(hidden)]
    pub fn replay_unmoved(&mut self, real: &EmuStepper) -> Option<Vec<Tile>> {
        let snapshot = self.ring.front()?.snapshot.clone();
        let inputs: [InputFrame; REPLAY_FRAMES] = std::array::from_fn(|i| self.ring[i].input);
        if self.shadow.is_none() {
            self.shadow = Some(EmuStepper::open(&self.rom).ok()?);
        }
        let shadow = self.shadow.as_mut()?;
        shadow.restore(&snapshot).ok()?;
        shadow.copy_picture_config_from(real);
        shadow.set_tile_capture(true);
        for input in inputs {
            if shadow.latch_and_advance_frame(input, &mut Discard) == 0 {
                return None;
            }
        }
        Some(tiles_of(shadow))
    }

    /// Run the redraw machine from `snapshot` for one side.
    fn redraw(
        &mut self,
        real: &EmuStepper,
        snapshot: &MachineSnapshot,
        inputs: [InputFrame; REPLAY_FRAMES],
        side: Side,
    ) -> Option<Redrawn> {
        if self.shadow.is_none() {
            self.shadow = Some(EmuStepper::open(&self.rom).ok()?);
        }
        let shadow = self.shadow.as_mut()?;
        shadow.restore(snapshot).ok()?;
        shadow.copy_picture_config_from(real);
        shadow.set_tile_capture(true);
        let map = &self.map;
        let before: Vec<Slot> = slots(map, &|a| shadow.peek_bus(a));
        // Move the camera one page toward the object: both edges, as the game keeps them.
        for (page, low) in [(map.left_page, map.left_x), (map.right_page, map.right_x)] {
            let _ = low;
            let moved = shadow.peek_bus(page).wrapping_add(side.page_step());
            shadow.poke_bus(u16::try_from(page).ok()?, moved);
        }
        let mut oam_before_picture = [0u8; 256];
        let mut objects: Vec<Object> = Vec::new();
        let mut avoid: Vec<Avoid> = Vec::new();
        for (i, input) in inputs.iter().enumerate() {
            if shadow.latch_and_advance_frame(*input, &mut Discard) == 0 {
                return None;
            }
            if i == 0 {
                // Where the objects that were alive when the camera moved are now, on the moved camera.
                let after = slots(map, &|a| shadow.peek_bus(a));
                // A slot the game freed and refilled during that frame is a different object: it must be
                // where the old one was, moved by the camera and a frame of its own movement.
                let moved = side.camera_pages() * 256;
                objects = after
                    .iter()
                    .filter(|s| {
                        before
                            .iter()
                            .any(|b| b.index == s.index && (s.rel - (b.rel - moved)).abs() <= 24)
                    })
                    .filter(|s| (-16..=272).contains(&s.rel))
                    .map(|s| Object { x: s.rel, y: s.y })
                    .collect();
                if let (Some(px), Some(py)) = (map.player_x, map.player_y) {
                    let (px, py) = (
                        i32::from(shadow.peek_bus(px)),
                        i32::from(shadow.peek_bus(py)),
                    );
                    avoid.push(Avoid {
                        x0: px - 16,
                        y0: py - 12,
                        x1: px + 16,
                        y1: py + 24,
                    });
                }
            }
            if i == REPLAY_FRAMES - 2 {
                oam_before_picture = *shadow.oam();
            }
        }
        self.frames_replayed += REPLAY_FRAMES as u64;
        if objects.is_empty() {
            return None;
        }
        let tiles = tiles_of(shadow);
        let kept = margin_draw::keep_near(&tiles, &objects, &avoid);

        let chr = shadow.chr();
        let strip = margin_draw::rasterise(
            &kept,
            HEIGHT,
            &|a| chr.get(a).copied().unwrap_or(0),
            shadow.palette(),
        );
        Some(Redrawn {
            strip,
            kept,
            oam_before_picture,
        })
    }
}

/// The sprite tiles the machine drew in the frame it just ran, console-free.
#[must_use]
pub fn tiles_of(machine: &EmuStepper) -> Vec<Tile> {
    machine
        .completed_sprites()
        .iter()
        .map(|d| Tile {
            x: d.x,
            y: d.y,
            tile: d.tile,
            base: d.base,
            palette: d.palette,
            flip_x: d.flip_x,
            flip_y: d.flip_y,
            oam_index: d.oam_index,
        })
        .collect()
}

/// One side's redraw.
struct Redrawn {
    strip: Vec<u8>,
    kept: Vec<Tile>,
    /// The redraw machine's OAM that drew its third frame (what `Shown::oam` is for the real one).
    oam_before_picture: [u8; 256],
}
