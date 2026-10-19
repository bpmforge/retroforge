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
/// Frames a vanished player is looked for near where it was (by position).
const ADOPT_GAP: u32 = 12;
/// How much further per unseen frame (px) that search reaches.
const ADOPT_PER_FRAME: i32 = 6;
/// Frames an entity taken up by position has to show it answers the pad.
const PROBATION: u32 = 90;
/// Opportunities the relaxed bar asks of a track once a player has been seen
/// (a respawned player is a new track with a short history).
const RELAXED_OPPS: u16 = 4;
/// Share of them it must answer.
const RELAXED_RATIO: f32 = 0.75;
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
    /// Its entity was seen THIS frame. `false` only on a player from
    /// [`PlayerTracker::tracked`]: the entity is where it was last seen
    /// (dead, behind a transition, off screen).
    pub visible: bool,
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
    /// The player's Y (feet) read from RAM by a profile, when it has one.
    pub hint_y: Option<i64>,
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
#[derive(Debug, Clone, Copy, Default)]
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

    fn add_all(&mut self, o: &Votes) {
        self.grav = self.grav.saturating_add(o.grav);
        self.anti = self.anti.saturating_add(o.anti);
        self.steady = self.steady.saturating_add(o.steady);
        self.inert_y = self.inert_y.saturating_add(o.inert_y);
        self.x_hits = self.x_hits.saturating_add(o.x_hits);
    }
}

/// The name, kept apart from the tracks: it follows the player through a
/// merge with a neighbour, a gap and a respawn.
#[derive(Debug, Clone, Copy)]
struct Lock {
    player: Player,
    /// Screen step of the last seen frame pair.
    vel: (i32, i32),
    /// Frames since the entity was seen.
    unseen: u32,
    /// Taken up by position (not earned by the pad): on probation.
    adopted: bool,
    since_adopt: u32,
    /// Where the name last rested on evidence rather than position.
    last_earned: Entity,
    /// Its sprites, to pick them out of a merged cluster.
    members: [Member; MEMBERS],
    n_members: usize,
    /// The followed track's counters when last read, and whose they are.
    /// The player's own windows (position by position, whatever track it
    /// is on): the pad evidence and the vertical dynamics.
    pt: Track,
    /// RAM hint minus screen position + camera: where the hint sits on the
    /// entity (learned while both are seen).
    off: (Option<i64>, Option<i64>),
}

/// `e` is no bigger than the player was: a neighbour merged into it is.
fn compat(e: &Entity, w: i32, h: i32) -> bool {
    e.width() <= w + (w / 4).max(4) && e.height() <= h + (h / 4).max(4)
}

/// Share of `pred`'s area that `e` covers, in percent.
fn covers(e: &Entity, pred: &Entity) -> i32 {
    let ow = (e.x1.min(pred.x1) - e.x0.max(pred.x0)).max(0);
    let oh = (e.y1.min(pred.y1) - e.y0.max(pred.y0)).max(0);
    ow * oh * 100 / (pred.width() * pred.height()).max(1)
}

fn shifted(mut e: Entity, dx: i32, dy: i32) -> Entity {
    e.x0 += dx;
    e.x1 += dx;
    e.y0 += dy;
    e.y1 += dy;
    e
}

/// Sprites of the player kept to pick them out of a merged cluster.
const MEMBERS: usize = 16;
/// How far (px) a member may have moved from where the step puts it.
const MEMBER_PX: i32 = 4;

/// One sprite of the player: offset from the entity's top-left, and size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Member {
    dx: i16,
    dy: i16,
    w: u8,
    h: u8,
}

/// The sprites that sit inside `e`: its members.
fn members_of(sprites: &[SpriteBox], e: &Entity) -> ([Member; MEMBERS], usize) {
    let mut out = [Member::default(); MEMBERS];
    let mut n = 0;
    for s in sprites.iter().take(128) {
        let (x0, y0) = (i32::from(s.x), i32::from(s.y));
        let inside =
            x0 >= e.x0 && y0 >= e.y0 && x0 + i32::from(s.w) <= e.x1 && y0 + i32::from(s.h) <= e.y1;
        if inside && n < MEMBERS {
            out[n] = Member {
                dx: (x0 - e.x0) as i16,
                dy: (y0 - e.y0) as i16,
                w: s.w,
                h: s.h,
            };
            n += 1;
        }
    }
    (out, n)
}

/// The player's own sprites out of a cluster it has merged into: for each
/// remembered member, the same-sized sprite where the step puts it. At
/// least half of them must be found.
fn carve(
    sprites: &[SpriteBox],
    pred: &Entity,
    members: &[Member],
) -> Option<(Entity, [Member; MEMBERS], usize)> {
    let mut used = [false; 128];
    let mut out: Option<Entity> = None;
    let mut found = 0;
    for m in members {
        let (wx, wy) = (pred.x0 + i32::from(m.dx), pred.y0 + i32::from(m.dy));
        let best = sprites
            .iter()
            .take(128)
            .enumerate()
            .filter(|(i, s)| !used[*i] && s.w == m.w && s.h == m.h)
            .map(|(i, s)| {
                let d = (i32::from(s.x) - wx).abs().max((i32::from(s.y) - wy).abs());
                (i, s, d)
            })
            .filter(|(_, _, d)| *d <= MEMBER_PX)
            .min_by_key(|(_, _, d)| *d);
        let Some((i, s, _)) = best else { continue };
        used[i] = true;
        found += 1;
        let (x0, y0) = (i32::from(s.x), i32::from(s.y));
        let (x1, y1) = (x0 + i32::from(s.w), y0 + i32::from(s.h));
        let e = out.get_or_insert(Entity {
            x0,
            y0,
            x1,
            y1,
            sprites: 0,
            look: Fnv64::new().finish(),
        });
        e.x0 = e.x0.min(x0);
        e.y0 = e.y0.min(y0);
        e.x1 = e.x1.max(x1);
        e.y1 = e.y1.max(y1);
        e.sprites = e.sprites.saturating_add(1);
        let mut h = Fnv64::new();
        h.u64(e.look).u64(u64::from(s.tile));
        e.look = h.finish();
    }
    let e = out.filter(|_| found * 2 >= members.len())?;
    let (m, n) = members_of(sprites, &e);
    // Neighbours inside the box are not the player's; keep only the matched
    // ones' shape: members_of over the carved box is the same set.
    Some((e, m, n))
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
    /// The player, when seen this frame.
    player: Option<Player>,
    /// The name, held while the player is out of sight.
    lock: Option<Lock>,
    /// The first player named, kept when its track is lost.
    first_found: Option<Player>,
    /// The player's vertical evidence, kept when its track is lost.
    /// Vertical evidence of players no longer followed.
    votes: Votes,
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
            lock: None,
            first_found: None,
            votes: Votes::default(),
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

    /// The player, when it is on screen this frame. While it is out of sight
    /// this is `None` and [`PlayerTracker::tracked`] still names it.
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
        self.follow(f.sprites, (f.hint_x, f.hint_y), cut);
        if self.lock.is_none_or(|l| l.unseen > 0) {
            self.acquire();
        }
        self.player = self.lock.filter(|l| l.player.visible).map(|l| l.player);
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
            let swung = |a: i32, b: i32| (a - b).abs() > 8 && (a * 2 > b * 3 || b * 2 > a * 3);
            if swung(e.width(), t.ent.width()) || swung(e.height(), t.ent.height()) {
                // It merged with (or split from) a neighbour: what it showed
                // before was another shape's motion.
                *t = Track {
                    live: true,
                    id: t.id,
                    ..Track::EMPTY
                };
            }
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

    /// The tracks' bar for a name: strict until a player has been seen, then
    /// relaxed (a respawned player is a new track with a short history).
    fn candidate(&self, t: &Track) -> Option<(f32, PlayerSource)> {
        if t.h_opps >= 4 && t.h_agree * 10 >= t.h_opps * 9 {
            return Some((2.0, PlayerSource::Hint));
        }
        if self.first_found.is_none() {
            return t.answers_pad().map(|s| (s, PlayerSource::Pad));
        }
        let ratio = |a: usize| f32::from(t.hits[a]) / f32::from(t.opps[a].max(1));
        let axis = (0..2)
            .filter(|&a| t.opps[a] >= RELAXED_OPPS && ratio(a) >= RELAXED_RATIO)
            .max_by(|&x, &y| ratio(x).total_cmp(&ratio(y)))?;
        let drifts = t.idle >= 8 && f32::from(t.drift) / f32::from(t.idle) > MAX_DRIFT_RATIO;
        let shows = t.free_hits[axis] >= 1 || t.look_changes >= 1;
        (!drifts && shows).then_some((ratio(axis), PlayerSource::Pad))
    }

    /// Name the track that answers the pad, when the player is unnamed or
    /// out of sight.
    fn acquire(&mut self) {
        let mut best: Option<(f32, PlayerSource, &Track)> = None;
        for t in self.tracks.iter().filter(|t| t.live && t.missed == 0) {
            if let Some((s, src)) = self.candidate(t) {
                // Nearer the middle of the picture wins a tie.
                let bias = 1.0 - (t.ent.cx() - SCREEN.0 / 2).abs() as f32 / 1024.0;
                let rank = s + bias * 0.01;
                if best.is_none_or(|b| rank > b.0) {
                    best = Some((rank, src, t));
                }
            }
        }
        let Some((_, source, t)) = best else { return };
        if let Some(old) = self.lock {
            self.votes.add_all(&Votes::of(&old.pt));
        }
        let p = Player {
            track: t.id,
            entity: t.ent,
            source,
            found_at: self.frame,
            score: t.best_axis().map_or(1.0, |a| a.1),
            visible: true,
        };
        self.first_found.get_or_insert(p);
        let off = self.lock.map_or((None, None), |l| l.off);
        self.lock = Some(Lock {
            player: p,
            vel: (0, 0),
            unseen: 0,
            adopted: false,
            since_adopt: 0,
            last_earned: t.ent,
            members: [Member::default(); MEMBERS],
            n_members: 0,
            pt: *t,
            off,
        });
    }

    /// Follow the named player into this frame: its own track, else the
    /// sprites where it must be (a merge with a neighbour), else whatever
    /// took its place nearby, else hold the name and the last place.
    fn follow(&mut self, sprites: &[SpriteBox], hint: (Option<i64>, Option<i64>), cut: bool) {
        let Some(mut lk) = self.lock else { return };
        let (cx, cy) = self.cam;
        let last = lk.player.entity;
        let (pw, ph) = (last.width(), last.height());
        // Where it should be: its last step on, or the RAM hint's word.
        let step = if lk.unseen <= 2 { lk.vel } else { (0, 0) };
        let mut pred = shifted(last, step.0, step.1);
        let anchored_x = hint.0.zip(lk.off.0).is_some();
        if let (Some(h), Some(o)) = (hint.0, lk.off.0) {
            let want = i32::try_from(h + o - cx).unwrap_or(pred.cx());
            pred = shifted(pred, want - pred.cx(), 0);
        }
        if let (Some(h), Some(o)) = (hint.1, lk.off.1) {
            let want = i32::try_from(h + o - cy).unwrap_or(pred.y1);
            pred = shifted(pred, 0, want - pred.y1);
        }
        let own = self
            .tracks
            .iter()
            .find(|t| t.live && t.id == lk.player.track && t.missed == 0);
        let mut seen: Option<(Entity, Option<u32>)> = None;
        let mut carved = None;
        if let Some(t) = own.filter(|t| compat(&t.ent, pw, ph) && covers(&t.ent, &pred) >= 60) {
            seen = Some((t.ent, Some(t.id)));
        } else if let Some((e, m, n)) =
            carve(sprites, &pred, &lk.members[..lk.n_members]).filter(|(e, _, _)| compat(e, pw, ph))
        {
            // Merged with a neighbour: its own sprites out of the cluster.
            carved = Some((m, n));
            let host = self
                .tracks
                .iter()
                .filter(|t| t.live && t.missed == 0)
                .min_by_key(|t| (t.ent.cx() - e.cx()).abs().max((t.ent.y1 - e.y1).abs()));
            seen = Some((e, host.map(|t| t.id)));
        } else if lk.unseen <= ADOPT_GAP || anchored_x {
            let gap = i32::try_from(lk.unseen).unwrap_or(i32::MAX / 64);
            let reach = if anchored_x {
                MATCH_PX + pw
            } else {
                MATCH_PX + ADOPT_PER_FRAME * gap + pw / 2
            };
            let near = self
                .tracks
                .iter()
                .filter(|t| t.live && t.missed == 0 && t.id != lk.player.track)
                .filter(|t| {
                    anchored_x
                        || ((t.ent.width() - pw).abs() <= pw / 2 + 4
                            && (t.ent.height() - ph).abs() <= ph / 2 + 4)
                })
                .map(|t| {
                    (
                        t,
                        (t.ent.cx() - pred.cx())
                            .abs()
                            .max((t.ent.y1 - pred.y1).abs()),
                    )
                })
                .filter(|(_, d)| *d <= reach)
                .min_by_key(|(_, d)| *d);
            if let Some((t, _)) = near {
                seen = Some((t.ent, Some(t.id)));
                lk.adopted = true;
                lk.since_adopt = 0;
                self.votes.add_all(&Votes::of(&lk.pt));
                lk.pt = Track {
                    live: true,
                    ..Track::EMPTY
                };
            }
        }
        let Some((e, track)) = seen else {
            lk.unseen = lk.unseen.saturating_add(1);
            lk.player.visible = false;
            lk.pt.ring_len = 0;
            self.lock = Some(lk);
            return;
        };
        if lk.unseen == 0 {
            lk.vel = (e.cx() - last.cx(), e.y1 - last.y1);
        } else {
            lk.vel = (0, 0);
        }
        let (m, n) = carved.unwrap_or_else(|| members_of(sprites, &e));
        if n > 0 {
            lk.members = m;
            lk.n_members = n;
        }
        lk.unseen = 0;
        lk.player.entity = e;
        if !lk.adopted {
            lk.last_earned = e;
        }
        lk.player.visible = true;
        if let Some(h) = hint.0 {
            lk.off.0 = Some(i64::from(e.cx()) + cx - h);
        }
        if let Some(h) = hint.1 {
            lk.off.1 = Some(i64::from(e.y1) + cy - h);
        }
        if let Some(id) = track {
            lk.player.track = id;
        }
        if cut {
            lk.pt.ring_len = 0;
        }
        lk.pt
            .push((e.cx() + cx as i32, e.y1 + cy as i32), (e.cx(), e.y1));
        if lk.pt.full() && self.pad_len == WINDOW {
            let pads = self.pads;
            score_window(&mut lk.pt, &pads);
        }
        if lk.adopted {
            lk.since_adopt += 1;
            if lk.since_adopt >= PROBATION {
                lk.adopted = false;
                let answers = (0..2).any(|a| {
                    lk.pt.opps[a] >= RELAXED_OPPS
                        && f32::from(lk.pt.hits[a]) / f32::from(lk.pt.opps[a]) >= 0.5
                });
                if lk.pt.opps[0] + lk.pt.opps[1] >= 6 && !answers {
                    // It never answered the pad: not the player. The name
                    // stays (the real one is out of sight, or not found
                    // yet) and the search for it opens again.
                    lk.player.visible = false;
                    lk.unseen = ADOPT_GAP + 1;
                    lk.player.track = 0;
                    lk.player.entity = lk.last_earned;
                    lk.pt.ring_len = 0;
                }
            }
        }
        self.lock = Some(lk);
    }

    /// The player, seen or not: where it was last seen while it is out of
    /// sight (`visible == false`): dead, behind a transition, off screen.
    #[must_use]
    pub fn tracked(&self) -> Option<&Player> {
        self.lock.as_ref().map(|l| &l.player)
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

    /// The vertical evidence of every player followed, once one was named.
    fn all_votes(&self) -> Option<Votes> {
        self.first_found?;
        let mut v = self.votes;
        if let Some(l) = &self.lock {
            v.add_all(&Votes::of(&l.pt));
        }
        Some(v)
    }

    /// The player's vertical evidence so far: gravity windows, windows
    /// against it, constant-speed windows, and Up/Down-held-but-still ones.
    #[must_use]
    pub fn vertical_votes(&self) -> Option<[u16; 4]> {
        let v = self.all_votes()?;
        Some([v.grav, v.anti, v.steady, v.inert_y])
    }

    /// What the camera looks like, so far.
    #[must_use]
    pub fn orientation(&self) -> OrientationGuess {
        if let Some(m) = self.mode_hint.filter(|m| *m != Orientation::Unknown) {
            return OrientationGuess {
                orientation: m,
                confidence: 3,
                source: OrientationSource::Hint,
            };
        }
        self.observed_orientation()
    }

    /// The call the player's own movement and the scroll axis make, whatever
    /// a hint says (ticket W34-07: a remembered call stands in only until
    /// this one is sure).
    #[must_use]
    pub fn observed_orientation(&self) -> OrientationGuess {
        let guess = |orientation, confidence, source| OrientationGuess {
            orientation,
            confidence,
            source,
        };
        let (tx, ty) = self.travel;
        let x_scroll = tx >= SCROLL_AXIS_PX && ty <= SCROLL_OTHER_PX;
        let y_scroll = ty >= SCROLL_AXIS_PX && tx <= SCROLL_OTHER_PX;
        // No sprite player (a game that draws its hero as background, or one
        // not yet found): the scroll axis is all there is.
        let Some(t) = self.all_votes() else {
            return match (x_scroll, y_scroll) {
                (true, _) => guess(Orientation::SideView, 1, OrientationSource::ScrollAxis),
                (_, true) => guess(Orientation::TopDown, 1, OrientationSource::ScrollAxis),
                _ => OrientationGuess::default(),
            };
        };
        // A jump's takeoff and landing are impulses that vote the other way for a
        // few windows each; the fall between them out-votes both.
        let falls = t.grav >= MIN_VOTES && t.grav.saturating_mul(5) >= t.anti.saturating_mul(4);
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
                    hint_y: None,
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
                hint_y: None,
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
    fn the_observed_call_ignores_a_hint_the_plain_call_obeys() {
        let mut tr = PlayerTracker::new();
        tr.set_mode_hint(Some(Orientation::TopDown));
        assert_eq!(tr.orientation().orientation, Orientation::TopDown);
        assert_eq!(tr.orientation().source, OrientationSource::Hint);
        // Nothing has been watched: the evidence says nothing yet.
        assert_eq!(tr.observed_orientation(), OrientationGuess::default());
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
                hint_y: None,
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
                hint_y: None,
            });
        }
        assert!(tr.player().is_none(), "nothing on screen is not seen");
        let held = tr.tracked().expect("the name is held");
        assert!(!held.visible);
        assert_eq!(tr.first_found().copied(), first);
        assert_eq!(tr.orientation(), before);
    }

    fn frame(tr: &mut PlayerTracker, s: &[SpriteBox], pad: Dirs, hx: Option<i64>, hy: Option<i64>) {
        tr.observe(&FrameInput {
            sprites: s,
            pad,
            camera: Some((0, 0)),
            hint_x: hx,
            hint_y: hy,
        });
    }

    /// The player at `x` (two 8x16 sprites, animating), an enemy at `ex`.
    fn pair(x: i16, ex: i16, tile: u16) -> Vec<SpriteBox> {
        vec![
            sb(x, 100, 8, 16, tile),
            sb(x + 8, 100, 8, 16, 9),
            sb(ex, 108, 16, 16, 20),
        ]
    }

    fn named_walker() -> (PlayerTracker, i16) {
        let mut tr = PlayerTracker::new();
        let mut x = 20i16;
        for i in 0..40u16 {
            x += 2;
            frame(&mut tr, &pair(x, 230, i / 4 % 3), R, None, None);
        }
        assert!(tr.player().is_some(), "named before the test starts");
        (tr, x)
    }

    #[test]
    fn walking_into_an_enemy_keeps_the_player_and_does_not_swallow_it() {
        let (mut tr, mut x) = named_walker();
        let track = tr.tracked().unwrap().track;
        // The enemy stands right where the player is walking: the clusters
        // touch and merge into one entity for a few frames.
        for i in 0..30u16 {
            x += 2;
            let s = pair(x, x + 16, i / 4 % 3);
            frame(&mut tr, &s, R, None, None);
            let p = tr.player().expect("still seen while merged");
            assert_eq!(p.entity.width(), 16, "carved out of the cluster: {p:?}");
            assert_eq!(p.entity.x0, i32::from(x));
        }
        let _ = track;
    }

    #[test]
    fn a_player_that_vanishes_and_reappears_nearby_is_the_same_player() {
        let (mut tr, x) = named_walker();
        let first = tr.first_found().copied().unwrap();
        // Two frames with nothing, then it is back a few pixels on.
        for _ in 0..2 {
            frame(&mut tr, &[], N, None, None);
            assert!(tr.player().is_none());
            assert!(tr.tracked().is_some());
        }
        frame(&mut tr, &pair(x + 6, 230, 1), N, None, None);
        let p = tr.player().expect("back");
        assert_eq!(p.entity.x0, i32::from(x) + 6);
        assert_eq!(p.found_at, first.found_at, "the same name, not a new find");
    }

    #[test]
    fn a_respawned_player_is_found_again_quickly_and_the_old_name_is_replaced() {
        let (mut tr, _) = named_walker();
        for _ in 0..200 {
            frame(&mut tr, &[], N, None, None);
        }
        assert!(tr.player().is_none());
        // It reappears at the far left and answers the pad.
        let mut x = 10i16;
        let mut frames = 0;
        while tr.player().is_none() && frames < 40 {
            x += 2;
            frame(&mut tr, &pair(x, 230, frames / 4 % 3), R, None, None);
            frames += 1;
        }
        let p = tr.player().expect("re-acquired");
        assert!(frames <= 16, "took {frames} frames");
        assert_eq!(p.entity.sprites, 2);
    }

    #[test]
    fn a_ram_hint_re_anchors_a_player_that_respawns_far_away() {
        let (mut tr, mut x) = named_walker();
        // The hint is the player's world X (offset 1000); learn it while seen.
        for _ in 0..4 {
            x += 2;
            frame(
                &mut tr,
                &pair(x, 230, 0),
                N,
                Some(i64::from(x) + 1000),
                Some(116 + 500),
            );
        }
        // It vanishes for 100 frames (longer than position search reaches)
        // while the RAM word keeps counting, then it is drawn at the hint.
        for _ in 0..100 {
            frame(&mut tr, &[], N, Some(1020), Some(616));
        }
        assert!(tr.player().is_none());
        // A same-sized decoy sits nearer to where it was last seen.
        let mut s = pair(150, 230, 1);
        s.extend([sb(x + 4, 100, 8, 16, 7), sb(x + 12, 100, 8, 16, 9)]);
        frame(&mut tr, &s, N, Some(1150), Some(616));
        let p = tr.player().expect("anchored");
        assert_eq!(
            p.entity.x0, 150,
            "the entity at the hint, not the decoy: {p:?}"
        );
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
                hint_y: None,
            });
        }
        assert!(tr.tracks.iter().filter(|t| t.live).count() <= MAX_TRACKS);
    }
}
