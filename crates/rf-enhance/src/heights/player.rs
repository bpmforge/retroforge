//! Entities, the player and the camera's orientation, by watching play
//! (ticket W34-02; `docs/design/HD2D_PLAN.md` section 2.1, S1 and S9).
//!
//! Clean-room: the only inputs are what the console shows and takes: the
//! sprites in OAM ([`SpriteBox`], decoded by the shell because this crate
//! may not read a console core), the D-pad, and the camera's scroll. No game
//! knowledge, no RAM, unless a profile supplies a hint.
//!
//! ## Entities
//!
//! [`cluster`] groups sprites that touch (or sit within [`CLUSTER_GAP`] px)
//! into one [`Entity`]: a Mario is two 8x16 sprites, a Link several 16x16.
//!
//! ## The player
//!
//! [`PlayerTracker`] follows each entity across frames (nearest centre in
//! screen space, so a scroll or screen cut does not lose it) and keeps a
//! fixed handful of counters per track: no history grows (CLAUDE.md law 8).
//! The player is the entity whose motion answers the pad: with a direction
//! held for [`WINDOW`] frames, it moved that way by at least [`MIN_MOVE_PX`]
//! over them. A world-fixed decoy never does; an enemy walking on its own
//! fails the idle test (it keeps moving with the pad released) and the held
//! direction when the script turns round. A sprite pinned to the screen
//! (RF-Scroller's sprite 0, a HUD icon) moves in the WORLD with the camera,
//! so it would pass a world-space test; it is excluded because it never moves
//! on SCREEN while the camera is still, or changes its look: the entity must
//! show screen-space response ([`MIN_FREE_HITS`]) or animation.
//!
//! Momentum (a skid after release) is why "released means stopped" is a
//! penalty with a generous bar and never a requirement.
//!
//! A profile may name the answer first ([`FrameInput::hint_x`], the player X
//! the W33-07 finder or a `[[scout.addr]]` row reads from RAM): the entity
//! whose world X moves with it is the player at once.
//!
//! ## Orientation
//!
//! Evidence, on the player's vertical motion in the world:
//!
//! - **gravity**: it speeds up downwards with Up/Down not held (a fall, the
//!   far half of a jump) - side view;
//! - **constant speed under Up/Down** - top-down;
//! - Up/Down held and the player does not move, while Left/Right does move
//!   it - weak side view; a scroll axis that is only X (or only Y) - weak;
//! - `[camera].mode` from the profile settles it ([`PlayerTracker::set_mode_hint`]).
//!
//! Confidence is the ladder of the other learners, 0 to 3.

use super::tile_grid::Fnv64;

/// Sprites closer than this many pixels (0 = touching) are one entity.
pub const CLUSTER_GAP: i32 = 1;
/// Tracks kept; a screen rarely holds more moving things the pad could mean.
pub const MAX_TRACKS: usize = 24;
/// Entities considered per frame (the SNES has 128 sprites).
pub const MAX_ENTITIES: usize = 64;
/// Frames the pad must hold one value before motion is compared with it.
pub const WINDOW: usize = 8;
/// A held direction must move the player at least this far over the window.
pub const MIN_MOVE_PX: i32 = 2;
/// A released pad with more travel than this over the window is "drift".
const DRIFT_PX: i32 = 4;
/// Opportunities (frames with a full window of a held direction) before a
/// track can be named the player.
pub const MIN_OPPS: u16 = 6;
/// Share of opportunities on an axis a track must answer.
pub const MIN_HIT_RATIO: f32 = 0.8;
/// Share of released-pad windows a track may keep moving in.
pub const MAX_DRIFT_RATIO: f32 = 0.5;
/// Hits where the entity also moved on screen (the camera cannot have done it).
pub const MIN_FREE_HITS: u16 = 2;
/// A camera jump larger than this per frame is a cut, not a scroll.
const CUT_PX: i64 = 48;
/// A track not seen for this many frames is dropped.
const MAX_MISSED: u8 = 20;
/// How far (px) an entity may move between frames and still be the same one.
const MATCH_PX: i32 = 24;
/// Votes of gravity (or of constant speed) needed to call an orientation.
pub const MIN_VOTES: u16 = 6;
/// Camera travel (px) on one axis, with the other below [`SCROLL_OTHER_PX`],
/// that names a scroll axis.
const SCROLL_AXIS_PX: i64 = 64;
const SCROLL_OTHER_PX: i64 = 16;
/// Screen size assumed when a frame does not say.
const SCREEN: (i32, i32) = (256, 240);

/// One sprite as the console shows it: screen pixels, top-left origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpriteBox {
    pub x: i16,
    pub y: i16,
    pub w: u8,
    pub h: u8,
    /// OAM priority puts it behind the background.
    pub behind_bg: bool,
    /// The tile number, to tell a changed look from the same one.
    pub tile: u16,
}

/// A group of touching sprites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Entity {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub sprites: u8,
    /// Hash of the members' tiles in order: changes when it animates.
    pub look: u64,
}

impl Entity {
    #[must_use]
    pub const fn width(&self) -> i32 {
        self.x1 - self.x0
    }

    #[must_use]
    pub const fn height(&self) -> i32 {
        self.y1 - self.y0
    }

    /// Centre X, in screen pixels.
    #[must_use]
    pub const fn cx(&self) -> i32 {
        (self.x0 + self.x1) / 2
    }
}

fn touches(a: &SpriteBox, b: &SpriteBox) -> bool {
    let (ax, ay, bx, by) = (
        i32::from(a.x),
        i32::from(a.y),
        i32::from(b.x),
        i32::from(b.y),
    );
    ax < bx + i32::from(b.w) + CLUSTER_GAP
        && bx < ax + i32::from(a.w) + CLUSTER_GAP
        && ay < by + i32::from(b.h) + CLUSTER_GAP
        && by < ay + i32::from(a.h) + CLUSTER_GAP
}

fn root(parent: &mut [u8], mut i: usize) -> usize {
    // Each step moves to a strictly smaller index or stops, so it ends.
    while usize::from(parent[i]) != i {
        let p = usize::from(parent[i]);
        parent[i] = parent[p];
        i = usize::from(parent[i]);
    }
    i
}

/// Group `sprites` (at most 128 are read) into entities, appended to `out`
/// (cleared first), at most [`MAX_ENTITIES`] of them.
pub fn cluster(sprites: &[SpriteBox], out: &mut Vec<Entity>) {
    out.clear();
    let n = sprites.len().min(128);
    let sprites = &sprites[..n];
    let mut parent = [0u8; 128];
    for (i, p) in parent.iter_mut().enumerate().take(n) {
        *p = i as u8;
    }
    for i in 0..n {
        for j in (i + 1)..n {
            if touches(&sprites[i], &sprites[j]) {
                let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                if ri != rj {
                    // The smaller index wins, so a root is always its group's first member.
                    parent[ri.max(rj)] = ri.min(rj) as u8;
                }
            }
        }
    }
    let mut slot = [u8::MAX; 128];
    for (i, s) in sprites.iter().enumerate() {
        let r = root(&mut parent, i);
        let (x0, y0) = (i32::from(s.x), i32::from(s.y));
        let (x1, y1) = (x0 + i32::from(s.w), y0 + i32::from(s.h));
        let at = if slot[r] == u8::MAX {
            if out.len() >= MAX_ENTITIES {
                continue;
            }
            slot[r] = out.len() as u8;
            out.push(Entity {
                x0,
                y0,
                x1,
                y1,
                sprites: 0,
                look: Fnv64::new().finish(),
            });
            out.len() - 1
        } else {
            usize::from(slot[r])
        };
        let e = &mut out[at];
        e.x0 = e.x0.min(x0);
        e.y0 = e.y0.min(y0);
        e.x1 = e.x1.max(x1);
        e.y1 = e.y1.max(y1);
        e.sprites = e.sprites.saturating_add(1);
        let mut h = Fnv64::new();
        h.u64(e.look).u64(u64::from(s.tile));
        e.look = h.finish();
    }
}

/// The four directions of the D-pad, console-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dirs {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
}

impl Dirs {
    /// `-1`, `0` or `1` along X.
    #[must_use]
    pub fn x(self) -> i8 {
        i8::from(self.right) - i8::from(self.left)
    }

    /// `-1`, `0` or `1` along Y (down is positive, as on screen).
    #[must_use]
    pub fn y(self) -> i8 {
        i8::from(self.down) - i8::from(self.up)
    }
}

/// How the camera sees the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    TopDown,
    SideView,
    #[default]
    Unknown,
}

/// What an orientation call rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrientationSource {
    #[default]
    None,
    /// The profile's `[camera].mode`.
    Hint,
    /// The player fell with Up/Down not held.
    Gravity,
    /// Up/Down moved the player at a constant speed.
    ConstantSpeed,
    /// Up/Down did nothing while Left/Right moved the player.
    VerticalInert,
    /// Only the scroll axis.
    ScrollAxis,
}

/// The orientation and how sure we are (0 to 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OrientationGuess {
    pub orientation: Orientation,
    pub confidence: u8,
    pub source: OrientationSource,
}

/// How the player was named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerSource {
    /// A profile hint (RAM X) moved with the entity.
    Hint,
    /// The pad moved it.
    Pad,
}

/// The player, once found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Player {
    pub track: u32,
    pub entity: Entity,
    pub source: PlayerSource,
    /// Frames since the tracker started, when it was first named.
    pub found_at: u32,
    /// 0..=1: the share of opportunities it answered.
    pub score: f32,
}

/// One track's evidence (see [`PlayerTracker::track_evidence`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackEvidence {
    pub id: u32,
    pub entity: Entity,
    pub opps: [u16; 2],
    pub hits: [u16; 2],
    pub free_hits: [u16; 2],
    pub idle: u16,
    pub drift: u16,
    pub look_changes: u16,
}

/// One frame's observations.
#[derive(Debug, Clone, Copy)]
pub struct FrameInput<'a> {
    pub sprites: &'a [SpriteBox],
    pub pad: Dirs,
    /// The camera's world position (any origin), when known.
    pub camera: Option<(i64, i64)>,
    /// The player's X read from RAM by a profile, when it has one.
    pub hint_x: Option<i64>,
}

const NO_POS: (i32, i32) = (0, 0);

#[derive(Debug, Clone, Copy)]
struct Track {
    live: bool,
    id: u32,
    ent: Entity,
    /// Screen velocity of the last step, for matching.
    vel: (i32, i32),
    missed: u8,
    /// World centre X and bottom Y, the last [`WINDOW`] frames, oldest first.
    ring: [(i32, i32); WINDOW],
    /// Screen centre X and bottom Y, the same frames.
    sring: [(i32, i32); WINDOW],
    ring_len: u8,
    look_changes: u16,
    /// Per axis (0 = X, 1 = Y): opportunities, hits, hits that also moved on screen.
    opps: [u16; 2],
    hits: [u16; 2],
    free_hits: [u16; 2],
    /// X only: released-pad windows, and how many kept moving.
    idle: u16,
    drift: u16,
    /// Vertical evidence.
    grav: u16,
    anti: u16,
    steady: u16,
    inert_y: u16,
    /// Hint agreement: windows where either moved, and where they agreed.
    h_opps: u16,
    h_agree: u16,
}

impl Track {
    const EMPTY: Track = Track {
        live: false,
        id: 0,
        ent: Entity {
            x0: 0,
            y0: 0,
            x1: 0,
            y1: 0,
            sprites: 0,
            look: 0,
        },
        vel: NO_POS,
        missed: 0,
        ring: [NO_POS; WINDOW],
        sring: [NO_POS; WINDOW],
        ring_len: 0,
        look_changes: 0,
        opps: [0; 2],
        hits: [0; 2],
        free_hits: [0; 2],
        idle: 0,
        drift: 0,
        grav: 0,
        anti: 0,
        steady: 0,
        inert_y: 0,
        h_opps: 0,
        h_agree: 0,
    };

    fn push(&mut self, world: (i32, i32), screen: (i32, i32)) {
        if usize::from(self.ring_len) == WINDOW {
            self.ring.copy_within(1.., 0);
            self.sring.copy_within(1.., 0);
            self.ring[WINDOW - 1] = world;
            self.sring[WINDOW - 1] = screen;
        } else {
            let i = usize::from(self.ring_len);
            self.ring[i] = world;
            self.sring[i] = screen;
            self.ring_len += 1;
        }
    }

    fn full(&self) -> bool {
        usize::from(self.ring_len) == WINDOW
    }

    /// Travel over the window on `axis`, in the world and on screen.
    fn travel(&self, axis: usize) -> (i32, i32) {
        let pick = |p: (i32, i32)| if axis == 0 { p.0 } else { p.1 };
        (
            pick(self.ring[WINDOW - 1]) - pick(self.ring[0]),
            pick(self.sring[WINDOW - 1]) - pick(self.sring[0]),
        )
    }

    fn best_axis(&self) -> Option<(usize, f32)> {
        (0..2)
            .filter(|&a| {
                self.opps[a] >= MIN_OPPS
                    && f32::from(self.hits[a]) / f32::from(self.opps[a]) >= MIN_HIT_RATIO
            })
            .map(|a| (a, f32::from(self.hits[a]) / f32::from(self.opps[a])))
            .max_by(|x, y| x.1.total_cmp(&y.1))
    }

    /// Whether the pad moved it, by the module's rule.
    fn answers_pad(&self) -> Option<f32> {
        let (axis, ratio) = self.best_axis()?;
        let drifts =
            self.idle >= 8 && f32::from(self.drift) / f32::from(self.idle) > MAX_DRIFT_RATIO;
        let on_screen = self.free_hits[axis] >= MIN_FREE_HITS;
        let alive = self.look_changes >= 2 && self.hits[axis] >= 24;
        (!drifts && (on_screen || alive)).then_some(ratio)
    }
}

/// The player track's vertical evidence (see [`Track`]), kept past its life.
#[derive(Debug, Clone, Copy)]
struct Votes {
    grav: u16,
    anti: u16,
    steady: u16,
    inert_y: u16,
    x_hits: u16,
}

impl Votes {
    fn of(t: &Track) -> Self {
        Self {
            grav: t.grav,
            anti: t.anti,
            steady: t.steady,
            inert_y: t.inert_y,
            x_hits: t.hits[0],
        }
    }
}

/// The player detector and orientation, fed once per frame.
#[derive(Debug, Clone)]
pub struct PlayerTracker {
    tracks: [Track; MAX_TRACKS],
    next_id: u32,
    frame: u32,
    pads: [Dirs; WINDOW],
    pad_len: usize,
    hints: [i64; WINDOW],
    hint_len: usize,
    prev_cam: Option<(i64, i64)>,
    cam: (i64, i64),
    travel: (i64, i64),
    player: Option<Player>,
    /// The first player named, kept when its track is lost.
    first_found: Option<Player>,
    /// The player's vertical evidence, kept when its track is lost.
    votes: Option<Votes>,
    mode_hint: Option<Orientation>,
    scratch: Vec<Entity>,
    claimed: [bool; MAX_TRACKS],
}

impl Default for PlayerTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerTracker {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tracks: [Track::EMPTY; MAX_TRACKS],
            next_id: 1,
            frame: 0,
            pads: [Dirs::default(); WINDOW],
            pad_len: 0,
            hints: [0; WINDOW],
            hint_len: 0,
            prev_cam: None,
            cam: (0, 0),
            travel: (0, 0),
            player: None,
            first_found: None,
            votes: None,
            mode_hint: None,
            scratch: Vec::with_capacity(MAX_ENTITIES),
            claimed: [false; MAX_TRACKS],
        }
    }

    /// The profile's `[camera].mode`, when it has one (`None` clears it).
    pub fn set_mode_hint(&mut self, mode: Option<Orientation>) {
        self.mode_hint = mode;
    }

    /// Frames observed.
    #[must_use]
    pub const fn frames(&self) -> u32 {
        self.frame
    }

    /// The player, once found.
    #[must_use]
    pub const fn player(&self) -> Option<&Player> {
        self.player.as_ref()
    }

    /// The first player named, even if its track has since been lost (it
    /// walked behind a bigger cluster, or off screen).
    #[must_use]
    pub const fn first_found(&self) -> Option<&Player> {
        self.first_found.as_ref()
    }

    /// Camera travel so far (px, absolute, cuts excluded), X then Y.
    #[must_use]
    pub const fn scroll_travel(&self) -> (i64, i64) {
        self.travel
    }

    /// Fold one frame in.
    pub fn observe(&mut self, f: &FrameInput<'_>) {
        self.frame += 1;
        // Pad and hint windows.
        push_ring(&mut self.pads, &mut self.pad_len, f.pad);
        if let Some(h) = f.hint_x {
            push_ring(&mut self.hints, &mut self.hint_len, h);
        } else {
            self.hint_len = 0;
        }
        // Camera.
        let cam = f.camera.unwrap_or(self.cam);
        let mut cut = false;
        if let Some(prev) = self.prev_cam {
            let (dx, dy) = (cam.0 - prev.0, cam.1 - prev.1);
            if dx.abs() > CUT_PX || dy.abs() > CUT_PX {
                cut = true;
            } else {
                self.travel.0 += dx.abs();
                self.travel.1 += dy.abs();
            }
        }
        self.prev_cam = Some(cam);
        self.cam = cam;

        cluster(f.sprites, &mut self.scratch);
        let ents = std::mem::take(&mut self.scratch);
        self.match_tracks(&ents);
        self.scratch = ents;
        // Update the matched tracks' windows and counters.
        let (cx, cy) = (cam.0 as i32, cam.1 as i32);
        let pads = self.pads;
        let pad_full = self.pad_len == WINDOW;
        let hint_full = self.hint_len == WINDOW;
        let hints = self.hints;
        for t in self.tracks.iter_mut().filter(|t| t.live && t.missed == 0) {
            let e = t.ent;
            if cut {
                // The world jumped: what came before cannot be compared.
                t.ring_len = 0;
            }
            t.push((e.cx() + cx, e.y1 + cy), (e.cx(), e.y1));
            if t.full() && pad_full {
                score_window(t, &pads);
                if hint_full {
                    score_hint(t, &hints);
                }
            }
        }
        self.pick_player();
    }

    fn match_tracks(&mut self, ents: &[Entity]) {
        self.claimed = [false; MAX_TRACKS];
        let mut taken = [false; MAX_ENTITIES];
        // Greedy nearest-first over (track, entity) pairs inside MATCH_PX.
        loop {
            let mut best: Option<(i32, usize, usize)> = None;
            for (ti, t) in self.tracks.iter().enumerate() {
                if !t.live || self.claimed[ti] {
                    continue;
                }
                let (px, py) = (t.ent.cx() + t.vel.0, t.ent.y1 + t.vel.1);
                for (ei, e) in ents.iter().enumerate() {
                    if taken[ei] {
                        continue;
                    }
                    let d = (e.cx() - px).abs().max((e.y1 - py).abs());
                    let limit = MATCH_PX + t.ent.width().max(e.width()) / 2;
                    if d <= limit && best.is_none_or(|b| d < b.0) {
                        best = Some((d, ti, ei));
                    }
                }
            }
            let Some((_, ti, ei)) = best else { break };
            self.claimed[ti] = true;
            taken[ei] = true;
            let t = &mut self.tracks[ti];
            let e = ents[ei];
            t.vel = (e.cx() - t.ent.cx(), e.y1 - t.ent.y1);
            if e.look != t.ent.look {
                t.look_changes = t.look_changes.saturating_add(1);
            }
            t.ent = e;
            t.missed = 0;
        }
        for (ti, t) in self.tracks.iter_mut().enumerate() {
            if t.live && !self.claimed[ti] {
                t.missed += 1;
                // A track out of sight loses its window: its position is a guess.
                t.ring_len = 0;
                if t.missed > MAX_MISSED {
                    *t = Track::EMPTY;
                }
            }
        }
        // Unmatched entities start tracks (the oldest free slot, else drop).
        for (ei, e) in ents.iter().enumerate() {
            if taken[ei] {
                continue;
            }
            if let Some(slot) = self.tracks.iter_mut().find(|t| !t.live) {
                *slot = Track {
                    live: true,
                    id: self.next_id,
                    ent: *e,
                    ..Track::EMPTY
                };
                self.next_id += 1;
            }
        }
    }

    fn pick_player(&mut self) {
        // A named player stays until its track is gone.
        if let Some(p) = self.player {
            if let Some(t) = self.tracks.iter().find(|t| t.live && t.id == p.track) {
                if t.missed == 0 {
                    self.player = Some(Player { entity: t.ent, ..p });
                }
                self.votes = Some(Votes::of(t));
                return;
            }
            // Lost (it walked behind a bigger cluster, or off screen): what it
            // showed stays in `votes`; a new track must earn the name afresh.
            self.player = None;
        }
        let mut best: Option<(f32, PlayerSource, &Track)> = None;
        for t in self.tracks.iter().filter(|t| t.live && t.missed == 0) {
            let hinted = t.h_opps >= 4 && t.h_agree * 10 >= t.h_opps * 9;
            let cand = if hinted {
                Some((1.0, PlayerSource::Hint))
            } else {
                t.answers_pad().map(|s| (s, PlayerSource::Pad))
            };
            if let Some((s, src)) = cand {
                // Nearer the middle of the picture wins a tie.
                let bias = 1.0 - (t.ent.cx() - SCREEN.0 / 2).abs() as f32 / 1024.0;
                let rank = s + if hinted { 1.0 } else { 0.0 } + bias * 0.01;
                if best.is_none_or(|b| rank > b.0) {
                    best = Some((rank, src, t));
                }
            }
        }
        if let Some((_, source, t)) = best {
            let p = Player {
                track: t.id,
                entity: t.ent,
                source,
                found_at: self.frame,
                score: t.best_axis().map_or(1.0, |a| a.1),
            };
            self.votes = Some(Votes::of(t));
            self.player = Some(p);
            self.first_found.get_or_insert(p);
        }
    }

    /// Every live track's evidence, for a readout:
    /// `(id, entity, opportunities [x, y], hits [x, y], on-screen hits [x, y], idle windows, drifting ones)`.
    pub fn track_evidence(&self) -> impl Iterator<Item = TrackEvidence> + '_ {
        self.tracks
            .iter()
            .filter(|t| t.live)
            .map(|t| TrackEvidence {
                id: t.id,
                entity: t.ent,
                opps: t.opps,
                hits: t.hits,
                free_hits: t.free_hits,
                idle: t.idle,
                drift: t.drift,
                look_changes: t.look_changes,
            })
    }

    /// The player's vertical evidence so far: gravity windows, windows
    /// against it, constant-speed windows, and Up/Down-held-but-still ones.
    #[must_use]
    pub fn vertical_votes(&self) -> Option<[u16; 4]> {
        let v = self.votes?;
        Some([v.grav, v.anti, v.steady, v.inert_y])
    }

    /// What the camera looks like, so far.
    #[must_use]
    pub fn orientation(&self) -> OrientationGuess {
        let guess = |orientation, confidence, source| OrientationGuess {
            orientation,
            confidence,
            source,
        };
        if let Some(m) = self.mode_hint.filter(|m| *m != Orientation::Unknown) {
            return guess(m, 3, OrientationSource::Hint);
        }
        let (tx, ty) = self.travel;
        let x_scroll = tx >= SCROLL_AXIS_PX && ty <= SCROLL_OTHER_PX;
        let y_scroll = ty >= SCROLL_AXIS_PX && tx <= SCROLL_OTHER_PX;
        // No sprite player (a game that draws its hero as background, or one
        // not yet found): the scroll axis is all there is.
        let Some(t) = self.votes else {
            return match (x_scroll, y_scroll) {
                (true, _) => guess(Orientation::SideView, 1, OrientationSource::ScrollAxis),
                (_, true) => guess(Orientation::TopDown, 1, OrientationSource::ScrollAxis),
                _ => OrientationGuess::default(),
            };
        };
        // A jump's takeoff and landing are impulses that vote the other way for a
        // few windows each; the fall between them out-votes both.
        let falls = t.grav >= MIN_VOTES && t.grav > t.anti;
        let steady = t.steady >= MIN_VOTES && t.steady >= t.grav.saturating_mul(2);
        if falls && !steady {
            let more = u8::from(x_scroll) + u8::from(t.grav >= MIN_VOTES * 3);
            return guess(
                Orientation::SideView,
                (2 + more).min(3),
                OrientationSource::Gravity,
            );
        }
        if steady && !falls {
            let more = u8::from(y_scroll || (tx > SCROLL_AXIS_PX && ty > SCROLL_AXIS_PX))
                + u8::from(t.steady >= MIN_VOTES * 3);
            return guess(
                Orientation::TopDown,
                (2 + more).min(3),
                OrientationSource::ConstantSpeed,
            );
        }
        if t.inert_y >= MIN_VOTES && t.steady == 0 && t.x_hits >= MIN_OPPS {
            return guess(
                Orientation::SideView,
                1 + u8::from(x_scroll),
                OrientationSource::VerticalInert,
            );
        }
        if x_scroll && t.steady == 0 {
            return guess(Orientation::SideView, 1, OrientationSource::ScrollAxis);
        }
        if y_scroll {
            return guess(Orientation::TopDown, 1, OrientationSource::ScrollAxis);
        }
        OrientationGuess::default()
    }
}

fn push_ring<T: Copy>(ring: &mut [T; WINDOW], len: &mut usize, v: T) {
    if *len == WINDOW {
        ring.copy_within(1.., 0);
        ring[WINDOW - 1] = v;
    } else {
        ring[*len] = v;
        *len += 1;
    }
}

/// Compare a track's window of motion with the pad's, when the pad held one
/// value on an axis for the whole window.
fn score_window(t: &mut Track, pads: &[Dirs; WINDOW]) {
    for axis in 0..2 {
        let dir = |d: Dirs| if axis == 0 { d.x() } else { d.y() };
        let d0 = dir(pads[0]);
        if pads.iter().any(|p| dir(*p) != d0) {
            continue;
        }
        let (world, screen) = t.travel(axis);
        if d0 != 0 {
            t.opps[axis] = t.opps[axis].saturating_add(1);
            if world.signum() == i32::from(d0) && world.abs() >= MIN_MOVE_PX {
                t.hits[axis] = t.hits[axis].saturating_add(1);
                if screen.signum() == i32::from(d0) && screen.abs() >= MIN_MOVE_PX {
                    t.free_hits[axis] = t.free_hits[axis].saturating_add(1);
                }
            } else if axis == 1 && world.abs() < MIN_MOVE_PX {
                // Up or Down held and nothing happened: vertical is inert.
                t.inert_y = t.inert_y.saturating_add(1);
            }
        } else if axis == 0 {
            t.idle = t.idle.saturating_add(1);
            if world.abs() >= DRIFT_PX {
                t.drift = t.drift.saturating_add(1);
            }
        }
    }
    // Vertical dynamics. Spans of 4 frames at both ends of the window.
    let y = |i: usize| t.ring[i].1;
    let (v1, v2) = (y(3) - y(0), y(WINDOW - 1) - y(4));
    let vertical_held = pads.iter().any(|p| p.y() != 0);
    if vertical_held {
        // Constant speed: both spans equal and clearly moving (px per 3 frames).
        let held = pads.iter().all(|p| p.y() == pads[0].y());
        if held && v1.signum() == v2.signum() && v1.abs() >= 3 && (v2 - v1).abs() <= 1 {
            t.steady = t.steady.saturating_add(1);
        }
    } else {
        let a = v2 - v1;
        if a >= 3 {
            t.grav = t.grav.saturating_add(1);
        } else if a <= -3 {
            t.anti = t.anti.saturating_add(1);
        }
    }
}

/// Compare a track's world X travel with the hint's over the window.
fn score_hint(t: &mut Track, hints: &[i64; WINDOW]) {
    let (world, _) = t.travel(0);
    let hint = hints[WINDOW - 1] - hints[0];
    let hint = i32::try_from(hint).unwrap_or(0);
    if hint.abs() < MIN_MOVE_PX && world.abs() < MIN_MOVE_PX {
        return;
    }
    t.h_opps = t.h_opps.saturating_add(1);
    if (world - hint).abs() <= 1 {
        t.h_agree = t.h_agree.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sb(x: i16, y: i16, w: u8, h: u8, tile: u16) -> SpriteBox {
        SpriteBox {
            x,
            y,
            w,
            h,
            behind_bg: false,
            tile,
        }
    }

    #[test]
    fn touching_sprites_make_one_entity_and_a_gap_makes_two() {
        let s = [
            sb(10, 10, 8, 16, 1),
            sb(18, 10, 8, 16, 2),
            sb(60, 10, 8, 8, 3),
        ];
        let mut out = Vec::new();
        cluster(&s, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].x0, out[0].x1, out[0].sprites), (10, 26, 2));
        assert_eq!(out[1].sprites, 1);
    }

    #[test]
    fn a_chain_of_sprites_joins_through_the_middle_one() {
        // Listed so that the first and last only meet through the second.
        let s = [
            sb(0, 0, 8, 8, 0),
            sb(40, 0, 8, 8, 0),
            sb(8, 0, 8, 8, 0),
            sb(16, 0, 24, 8, 0),
        ];
        let mut out = Vec::new();
        cluster(&s, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sprites, 4);
    }

    /// A synthetic play session: a player that follows the pad (with a
    /// scroll once it passes the middle), an enemy that walks on its own, a
    /// world-fixed decoy and a HUD box pinned to the screen. `jump` makes the
    /// player fall under gravity now and then; `top_down` lets Up/Down move
    /// it at 2 px a frame instead.
    struct Sim {
        px: i32,
        py: i32,
        vy: i32,
        cam: i64,
        enemy: i32,
        top_down: bool,
        t: u32,
    }

    impl Sim {
        fn new(top_down: bool) -> Self {
            Self {
                px: 40,
                py: 120,
                vy: 0,
                cam: 0,
                enemy: 200,
                top_down,
                t: 0,
            }
        }

        fn step(&mut self, pad: Dirs) -> Vec<SpriteBox> {
            self.t += 1;
            self.px += 2 * i32::from(pad.x());
            if self.top_down {
                self.py += 2 * i32::from(pad.y());
            } else {
                // Gravity: a jump starts every 40 frames and arcs for 24.
                let phase = self.t % 40;
                if phase == 1 {
                    self.vy = -6;
                }
                if (1..25).contains(&phase) {
                    self.vy += 1;
                    self.py += self.vy / 2;
                } else {
                    self.vy = 0;
                }
            }
            // The camera follows once he passes x = 120.
            if self.px > 120 {
                self.cam += i64::from(self.px - 120);
                self.px = 120;
            }
            self.enemy -= 1;
            let wx = |world: i32| (world as i64 - self.cam) as i32;
            vec![
                // The player: two touching sprites (look changes as he walks).
                sb(
                    self.px as i16,
                    self.py as i16,
                    8,
                    16,
                    (self.t / 4 % 3) as u16,
                ),
                sb(self.px as i16 + 8, self.py as i16, 8, 16, 9),
                // The enemy walks left at 1 px a frame in the world.
                sb(wx(self.enemy + 300) as i16, 150, 16, 16, 20),
                // A decoy fixed in the world.
                sb(wx(400) as i16, 100, 16, 16, 30),
                // A HUD box pinned to the screen.
                sb(8, 8, 8, 8, 40),
            ]
        }
    }

    fn run(sim: &mut Sim, script: &[(Dirs, u32)]) -> PlayerTracker {
        let mut tr = PlayerTracker::new();
        for (pad, n) in script {
            for _ in 0..*n {
                let s = sim.step(*pad);
                tr.observe(&FrameInput {
                    sprites: &s,
                    pad: *pad,
                    camera: Some((sim.cam, 0)),
                    hint_x: None,
                });
            }
        }
        tr
    }

    const R: Dirs = Dirs {
        left: false,
        right: true,
        up: false,
        down: false,
    };
    const L: Dirs = Dirs {
        left: true,
        right: false,
        up: false,
        down: false,
    };
    const U: Dirs = Dirs {
        left: false,
        right: false,
        up: true,
        down: false,
    };
    const D: Dirs = Dirs {
        left: false,
        right: false,
        up: false,
        down: true,
    };
    const N: Dirs = Dirs {
        left: false,
        right: false,
        up: false,
        down: false,
    };

    #[test]
    fn the_player_is_the_cluster_that_answers_the_pad_not_the_enemy_decoy_or_hud() {
        let mut sim = Sim::new(false);
        let tr = run(&mut sim, &[(N, 10), (R, 40), (N, 20), (L, 20), (N, 20)]);
        let p = tr.player().expect("player found");
        assert_eq!(p.source, PlayerSource::Pad);
        assert_eq!(p.entity.sprites, 2, "the two-sprite cluster: {p:?}");
        assert!(p.found_at <= 120, "found at {}", p.found_at);
        assert!(p.found_at >= 10 + WINDOW as u32);
    }

    #[test]
    fn a_pinned_hud_sprite_that_scrolls_in_the_world_is_not_the_player() {
        // Only a HUD box on screen, camera running right with the pad.
        let mut tr = PlayerTracker::new();
        for i in 0..80 {
            tr.observe(&FrameInput {
                sprites: &[sb(8, 8, 8, 8, 40)],
                pad: R,
                camera: Some((i * 2, 0)),
                hint_x: None,
            });
        }
        assert!(tr.player().is_none());
    }

    #[test]
    fn falling_under_gravity_is_side_view() {
        let mut sim = Sim::new(false);
        let tr = run(&mut sim, &[(R, 50), (N, 30), (L, 30), (U, 20), (N, 60)]);
        let o = tr.orientation();
        assert_eq!(o.orientation, Orientation::SideView, "{o:?}");
        assert_eq!(o.source, OrientationSource::Gravity);
        assert!(o.confidence >= 2);
    }

    #[test]
    fn constant_speed_under_up_down_is_top_down() {
        let mut sim = Sim::new(true);
        let tr = run(
            &mut sim,
            &[(R, 30), (N, 10), (D, 30), (U, 30), (L, 20), (N, 20)],
        );
        let o = tr.orientation();
        assert_eq!(o.orientation, Orientation::TopDown, "{o:?}");
        assert_eq!(o.source, OrientationSource::ConstantSpeed);
    }

    #[test]
    fn a_profile_mode_settles_orientation_at_once() {
        let mut tr = PlayerTracker::new();
        assert_eq!(tr.orientation().orientation, Orientation::Unknown);
        tr.set_mode_hint(Some(Orientation::SideView));
        let o = tr.orientation();
        assert_eq!((o.orientation, o.confidence), (Orientation::SideView, 3));
    }

    #[test]
    fn a_ram_hint_names_the_player_without_the_pad() {
        let mut tr = PlayerTracker::new();
        let mut x = 40i32;
        for _ in 0..40 {
            x += 2;
            // The pad is idle: only the hint says which one is the player.
            let s = [sb(x as i16, 100, 16, 16, 1), sb(200, 50, 16, 16, 2)];
            tr.observe(&FrameInput {
                sprites: &s,
                pad: N,
                camera: Some((0, 0)),
                hint_x: Some(i64::from(x) + 1000),
            });
        }
        let p = tr.player().expect("hinted");
        assert_eq!(p.source, PlayerSource::Hint);
        assert_eq!(p.entity.x0, x);
    }

    #[test]
    fn a_lost_player_keeps_its_name_and_its_orientation_evidence() {
        let mut sim = Sim::new(false);
        let mut tr = run(&mut sim, &[(R, 50), (N, 30), (L, 30), (N, 60)]);
        let (first, before) = (tr.first_found().copied(), tr.orientation());
        assert_eq!(before.orientation, Orientation::SideView);
        // Everything leaves the screen for longer than a track lives.
        for _ in 0..(MAX_MISSED as usize + 5) {
            tr.observe(&FrameInput {
                sprites: &[],
                pad: N,
                camera: None,
                hint_x: None,
            });
        }
        assert!(tr.player().is_none());
        assert_eq!(tr.first_found().copied(), first);
        assert_eq!(tr.orientation(), before);
    }

    #[test]
    fn tracks_and_entities_stay_bounded_on_a_screenful_of_sprites() {
        let many: Vec<SpriteBox> = (0..128)
            .map(|i| sb((i % 16 * 20) as i16, (i / 16 * 24) as i16, 8, 8, i as u16))
            .collect();
        let mut tr = PlayerTracker::new();
        for _ in 0..50 {
            tr.observe(&FrameInput {
                sprites: &many,
                pad: R,
                camera: None,
                hint_x: None,
            });
        }
        assert!(tr.tracks.iter().filter(|t| t.live).count() <= MAX_TRACKS);
    }
}
