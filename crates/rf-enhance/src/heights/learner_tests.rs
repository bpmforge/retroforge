//! The oracle: a synthetic side-view level with real falls, walls, a
//! platform, walk-through decoration and a cloud nobody visits, played by a
//! bot for 60 s. Scored against the level's own collision map.

use super::*;
use crate::heights::player::{Entity, PlayerSource};

const CELL: i32 = 16;
const COLS: i32 = 64;
const ROWS: i32 = 14;
const W: i32 = 12;
const H: i32 = 16;
const SKY: u64 = 1;
const GTOP: u64 = 2;
const FILL: u64 = 3;
const PIPE: u64 = 4;
const PLAT: u64 = 5;
const DECOR: u64 = 6;
const CLOUD: u64 = 7;

fn solid_key(k: u64) -> bool {
    matches!(k, GTOP | FILL | PIPE | PLAT)
}

fn level() -> Vec<u64> {
    let mut l = vec![SKY; (COLS * ROWS) as usize];
    let mut put = |c: i32, r: i32, k: u64| l[(r * COLS + c) as usize] = k;
    for c in 0..COLS {
        put(c, 11, GTOP);
        put(c, 12, FILL);
        put(c, 13, FILL);
    }
    for r in 9..=10 {
        put(20, r, PIPE);
        put(21, r, PIPE);
    }
    for c in 30..=33 {
        put(c, 9, PLAT);
    }
    for c in 40..=42 {
        put(c, 10, DECOR);
    }
    for c in 10..=12 {
        put(c, 2, CLOUD);
    }
    l
}

fn tile(k: u64, _layer: u8) -> TileCell {
    TileCell {
        pattern: k,
        palette: 1,
        flags: 0,
    }
}

struct Sim {
    lv: Vec<u64>,
    x: i32,
    y: i32,
    vy: i32,
    cam: i32,
    prev_x: i32,
    stuck: u32,
    t: u32,
    heading: i8,
}

impl Sim {
    fn new() -> Self {
        Sim {
            lv: level(),
            x: 40,
            y: 11 * CELL - H,
            vy: 0,
            cam: 0,
            prev_x: 0,
            stuck: 0,
            t: 0,
            heading: 1,
        }
    }

    fn at(&self, c: i32, r: i32) -> u64 {
        if !(0..COLS).contains(&c) || !(0..ROWS).contains(&r) {
            return if r >= ROWS { FILL } else { SKY };
        }
        self.lv[(r * COLS + c) as usize]
    }

    fn collides(&self, x: i32, y: i32) -> bool {
        for r in y.div_euclid(CELL)..=(y + H - 1).div_euclid(CELL) {
            for c in x.div_euclid(CELL)..=(x + W - 1).div_euclid(CELL) {
                if solid_key(self.at(c, r)) {
                    return true;
                }
            }
        }
        false
    }

    /// One frame of play; returns the pad.
    fn step(&mut self) -> Dirs {
        self.t += 1;
        if self.x >= COLS * CELL - 80 {
            self.heading = -1;
        }
        if self.x <= 40 {
            self.heading = 1;
        }
        self.stuck = if self.x == self.prev_x {
            self.stuck + 1
        } else {
            0
        };
        self.prev_x = self.x;
        let jump = self.t.is_multiple_of(97)
            || self.stuck == 15
            || (self.heading > 0 && self.x == 462)
            || (self.heading < 0 && self.x == 556);
        let dir = self.heading;
        let nx = self.x + i32::from(dir) * 2;
        if !self.collides(nx, self.y) {
            self.x = nx;
        }
        let grounded = self.collides(self.x, self.y + 1);
        if jump && grounded {
            self.vy = -9;
        }
        self.vy = (self.vy + 1).min(8);
        let ny = self.y + self.vy;
        if self.collides(self.x, ny) {
            if self.vy > 0 {
                self.y = (ny + H).div_euclid(CELL) * CELL - H;
            } else {
                self.y = ny.div_euclid(CELL) * CELL + CELL;
            }
            self.vy = 0;
        } else {
            self.y = ny;
        }
        self.cam = (self.x - 120).clamp(0, COLS * CELL - 256);
        Dirs {
            left: dir < 0,
            right: dir > 0,
            up: false,
            down: false,
        }
    }

    fn grid(&self, remap: &dyn Fn(u64) -> u64) -> TileGrid {
        let first = self.cam.div_euclid(CELL);
        let cols = 17;
        let cells = (0..ROWS)
            .flat_map(|r| (0..cols).map(move |c| (r, c)))
            .map(|(r, c)| tile(remap(self.at(first + c, r)), 0))
            .collect();
        TileGrid {
            layer: 0,
            cell_px: CELL as u8,
            hud: false,
            exact: true,
            rows_px: (0, 224),
            origin_world: (i64::from(first * CELL), 0),
            offset: ((self.cam - first * CELL) as u8, 0),
            cols: cols as u16,
            rows: ROWS as u16,
            cells,
        }
    }

    fn player(&self) -> Player {
        let x0 = self.x - self.cam;
        Player {
            track: 1,
            entity: Entity {
                x0,
                y0: self.y,
                x1: x0 + W,
                y1: self.y + H,
                sprites: 1,
                look: 0,
            },
            source: PlayerSource::Pad,
            found_at: 0,
            score: 1.0,
        }
    }
}

fn full_level_grid(lv: &[u64]) -> TileGrid {
    TileGrid {
        layer: 0,
        cell_px: CELL as u8,
        hud: false,
        exact: true,
        rows_px: (0, (ROWS * CELL) as u16),
        origin_world: (0, 0),
        offset: (0, 0),
        cols: COLS as u16,
        rows: ROWS as u16,
        cells: lv.iter().map(|&k| tile(k, 0)).collect(),
    }
}

fn play(frames: u32, signals: Signals, remap: &dyn Fn(u32, u64) -> u64) -> HeightLearner {
    let mut sim = Sim::new();
    let mut l = HeightLearner::new();
    l.set_signals(signals);
    for n in 0..frames {
        let pad = sim.step();
        let g = [sim.grid(&|k| remap(n, k))];
        let p = sim.player();
        l.observe(&FrameObs {
            grids: &g,
            sprites: &[],
            entities: &[p.entity],
            player: Some(&p),
            pad,
            orientation: Orientation::SideView,
            hidden: None,
        });
    }
    l
}

/// (precision, recall) of "drawn solid" against the collision map, over the
/// whole level's cells.
fn score(l: &HeightLearner) -> (f32, f32) {
    let lv = level();
    // The learner is asked about the level exactly as the game stores it, so
    // a shuffled run is judged on the true layout.
    let hg = l.grid_for(&full_level_grid(&lv));
    let (mut tp, mut fp, mut fneg) = (0u32, 0u32, 0u32);
    for (i, &k) in lv.iter().enumerate() {
        let learned = hg.class[i].is_solid();
        match (learned, solid_key(k)) {
            (true, true) => tp += 1,
            (true, false) => fp += 1,
            (false, true) => fneg += 1,
            _ => {}
        }
    }
    (
        tp as f32 / (tp + fp).max(1) as f32,
        tp as f32 / (tp + fneg).max(1) as f32,
    )
}

const SIXTY_SECONDS: u32 = 3600;

fn ident(_: u32, k: u64) -> u64 {
    k
}

#[test]
fn sixty_seconds_of_play_meets_the_bar() {
    let l = play(SIXTY_SECONDS, Signals::ALL, &ident);
    let (p, r) = score(&l);
    println!(
        "synthetic oracle: precision {p:.3}, recall {r:.3}, {:?}",
        l.stats()
    );
    assert!(p >= 0.90, "precision {p}");
    assert!(r >= 0.70, "recall {r}");
    let t = l.table();
    let class = |k| t.get(&key(k)).map(|e| e.learned.drawn().0);
    assert_eq!(class(PIPE), Some(Class::Wall));
    assert!(matches!(class(GTOP), Some(Class::Platform)));
    assert!(matches!(class(PLAT), Some(Class::Platform)));
    assert_eq!(class(SKY), Some(Class::Floor));
    assert_eq!(class(DECOR), Some(Class::Floor), "walked through");
    assert!(
        matches!(class(CLOUD), None | Some(Class::Unknown)),
        "never visited: flat, not guessed"
    );
}

fn key(k: u64) -> TileKey {
    TileKey {
        pattern: k,
        palette: 1,
        layer: 0,
        cell_px: CELL as u8,
    }
}

#[test]
fn without_support_the_recall_drops() {
    let l = play(
        SIXTY_SECONDS,
        Signals::ALL.without(Signals::S1_SUPPORT),
        &ident,
    );
    let (_, r) = score(&l);
    println!("S1 off: recall {r:.3}");
    assert!(r < 0.70, "S1 off should lose recall, got {r}");
}

#[test]
fn shuffled_keys_lose_precision_or_recall() {
    // Every frame the tile identities are scrambled: nothing can be learned.
    let l = play(SIXTY_SECONDS, Signals::ALL, &|n, k| {
        (k + u64::from(n.wrapping_mul(2_654_435_761) >> 13)) % 7 + 1
    });
    let (p, r) = score(&l);
    println!("shuffled: precision {p:.3}, recall {r:.3}");
    assert!(p < 0.90 || r < 0.70, "p {p} r {r}");
}

#[test]
fn the_same_play_gives_the_same_table_and_bytes() {
    let a = play(1500, Signals::ALL, &ident);
    let b = play(1500, Signals::ALL, &ident);
    assert_eq!(a.table(), b.table());
    assert_eq!(a.table().to_bytes(), b.table().to_bytes());
}

#[test]
fn unknown_is_flat_and_so_is_everything_below_the_draw_level() {
    let fresh = HeightLearner::new();
    let g = full_level_grid(&level());
    let hg = fresh.grid_for(&g);
    assert!(hg.class.iter().all(|c| *c == Class::Unknown));
    assert!(hg.depth.iter().all(|d| *d == 0));
    // Early in play (a few frames), nothing has reached the draw level.
    let early = play(40, Signals::ALL, &ident);
    let hg = early.grid_for(&g);
    assert!(hg.depth.iter().all(|d| *d == 0), "{:?}", early.stats());
}

#[test]
fn hud_grids_are_never_read_or_raised() {
    let mut sim = Sim::new();
    let mut l = HeightLearner::new();
    for _ in 0..600 {
        let pad = sim.step();
        let mut g = sim.grid(&|k| k);
        g.hud = true;
        let p = sim.player();
        l.observe(&FrameObs {
            grids: &[g],
            sprites: &[],
            entities: &[p.entity],
            player: Some(&p),
            pad,
            orientation: Orientation::SideView,
            hidden: None,
        });
    }
    assert!(l.table().is_empty(), "a HUD layer teaches nothing");
    let mut g = full_level_grid(&level());
    g.hud = true;
    assert!(l.grid_for(&g).depth.iter().all(|d| *d == 0));
}

#[test]
fn a_broken_player_track_leaves_no_false_landing() {
    // The same fall, but the track id changes on the landing frame: the
    // dynamics restart, so no `floor_top` is voted from the broken pair.
    let mut sim = Sim::new();
    let mut l = HeightLearner::new();
    let mut id = 1;
    for _ in 0..400 {
        let pad = sim.step();
        let g = [sim.grid(&|k| k)];
        let mut p = sim.player();
        // A new track every frame: nothing is ever continuous.
        id += 1;
        p.track = id;
        l.observe(&FrameObs {
            grids: &g,
            sprites: &[],
            entities: &[p.entity],
            player: Some(&p),
            pad,
            orientation: Orientation::SideView,
            hidden: None,
        });
    }
    let learned: u32 = l.table().iter().map(|(_, e)| e.evidence.floor_top).sum();
    assert_eq!(learned, 0, "no continuous track, no support evidence");
}

#[test]
fn an_unknown_orientation_learns_nothing_but_sightings() {
    let mut sim = Sim::new();
    let mut l = HeightLearner::new();
    for _ in 0..300 {
        let pad = sim.step();
        let g = [sim.grid(&|k| k)];
        let p = sim.player();
        l.observe(&FrameObs {
            grids: &g,
            sprites: &[],
            entities: &[p.entity],
            player: Some(&p),
            pad,
            orientation: Orientation::Unknown,
            hidden: None,
        });
    }
    assert_eq!(l.stats().drawn, 0);
}

#[test]
fn a_tile_that_hides_the_player_becomes_tall() {
    let mut l = HeightLearner::new();
    let mut g = TileGrid {
        layer: 0,
        cell_px: 16,
        hud: false,
        exact: true,
        rows_px: (0, 64),
        origin_world: (0, 0),
        offset: (0, 0),
        cols: 4,
        rows: 4,
        cells: vec![tile(SKY, 0); 16],
    };
    g.cells[5] = tile(PIPE, 0);
    for n in 0..200 {
        let p = Player {
            track: 1,
            entity: Entity {
                x0: 16 + (n % 3),
                y0: 16,
                x1: 28 + (n % 3),
                y1: 32,
                sprites: 1,
                look: 0,
            },
            source: PlayerSource::Pad,
            found_at: 0,
            score: 1.0,
        };
        l.observe(&FrameObs {
            grids: std::slice::from_ref(&g),
            sprites: &[],
            entities: &[p.entity],
            player: Some(&p),
            pad: Dirs::default(),
            orientation: Orientation::TopDown,
            hidden: Some((16, 16, 28, 32)),
        });
    }
    let e = l.table().get(&key(PIPE)).unwrap();
    assert_eq!(e.learned.class, Class::Tall, "{e:?}");
    assert!(e.learned.confidence >= Learned::DRAW_LEVEL);
}

#[test]
fn a_slower_layer_is_a_backdrop_plane() {
    let mut l = HeightLearner::new();
    let mk = |layer: u8, x: i64| TileGrid {
        layer,
        cell_px: 8,
        hud: false,
        exact: true,
        rows_px: (0, 32),
        origin_world: (x, 0),
        offset: (0, 0),
        cols: 4,
        rows: 4,
        cells: vec![tile(u64::from(layer) + 10, layer); 16],
    };
    for n in 0..300i64 {
        let g = [mk(0, n * 2), mk(1, n)];
        l.observe(&FrameObs {
            grids: &g,
            sprites: &[],
            entities: &[],
            player: None,
            pad: Dirs::default(),
            orientation: Orientation::SideView,
            hidden: None,
        });
    }
    let hills = l
        .table()
        .iter()
        .find(|(k, _)| k.layer == 1)
        .map(|(_, e)| e.learned)
        .unwrap();
    let ground = l
        .table()
        .iter()
        .find(|(k, _)| k.layer == 0)
        .map(|(_, e)| e.learned)
        .unwrap();
    assert_eq!(hills.class, Class::Backdrop);
    assert!(hills.confidence >= Learned::DRAW_LEVEL);
    assert_ne!(ground.class, Class::Backdrop);
    let hg = l.grid_for(&mk(1, 0));
    assert!(hg.depth.iter().all(|d| *d == 0), "set back, never extruded");
}

#[test]
fn walking_up_a_ramp_votes_a_slope() {
    // A static staircase rising one tile edge (16 px) every two columns; the
    // player walks 2 px and rises 1 px a frame, so dy/dx = 1/2.
    let mut cells = vec![tile(SKY, 0); 16 * 9];
    for c in 0..16usize {
        let surface = 112 - 8 * c as i32;
        for r in (surface / 16)..9 {
            cells[r as usize * 16 + c] = tile(9, 0);
        }
    }
    let g = TileGrid {
        layer: 0,
        cell_px: 16,
        hud: false,
        exact: true,
        rows_px: (0, 144),
        origin_world: (0, 0),
        offset: (0, 0),
        cols: 16,
        rows: 9,
        cells,
    };
    let mut l = HeightLearner::new();
    for n in 0..400i32 {
        let step = n % 100;
        let x0 = 8 + step * 2;
        let y1 = 112 - step - 4;
        let p = Player {
            track: 1,
            entity: Entity {
                x0,
                y0: y1 - 16,
                x1: x0 + 12,
                y1,
                sprites: 1,
                look: 0,
            },
            source: PlayerSource::Pad,
            found_at: 0,
            score: 1.0,
        };
        l.observe(&FrameObs {
            grids: std::slice::from_ref(&g),
            sprites: &[],
            entities: &[p.entity],
            player: Some(&p),
            pad: Dirs {
                right: true,
                ..Dirs::default()
            },
            orientation: Orientation::SideView,
            hidden: None,
        });
    }
    let e = l.table().get(&key(9)).expect("the surface was seen");
    println!("{e:?}");
    assert!(e.evidence.slope_n > 0 && e.evidence.slope > 0, "{e:?}");
    assert!(matches!(e.learned.class, Class::Ramp(r) if r > 0), "{e:?}");
}

#[test]
fn top_down_walls_are_the_tiles_he_keeps_walking_into() {
    let wall_col = 5usize;
    let mut cells = vec![tile(SKY, 0); 8 * 8];
    for r in 0..8 {
        cells[r * 8 + wall_col] = tile(PIPE, 0);
    }
    let g = TileGrid {
        layer: 0,
        cell_px: 16,
        hud: false,
        exact: true,
        rows_px: (0, 128),
        origin_world: (0, 0),
        offset: (0, 0),
        cols: 8,
        rows: 8,
        cells,
    };
    let mut l = HeightLearner::new();
    let at = |x0: i32| Player {
        track: 1,
        entity: Entity {
            x0,
            y0: 32,
            x1: x0 + 16,
            y1: 48,
            sprites: 1,
            look: 0,
        },
        source: PlayerSource::Pad,
        found_at: 0,
        score: 1.0,
    };
    let feed = |l: &mut HeightLearner, x0: i32, right: bool| {
        let p = at(x0);
        l.observe(&FrameObs {
            grids: std::slice::from_ref(&g),
            sprites: &[],
            entities: &[p.entity],
            player: Some(&p),
            pad: Dirs {
                right,
                ..Dirs::default()
            },
            orientation: Orientation::TopDown,
            hidden: None,
        });
    };
    // Six visits: walk up to the wall, push against it, back off.
    for _ in 0..6 {
        for x0 in (40..=64).step_by(2) {
            feed(&mut l, x0, true);
        }
        for _ in 0..40 {
            feed(&mut l, 64, true);
        }
        for x0 in (40..=64).rev().step_by(2) {
            feed(&mut l, x0, false);
        }
    }
    let wall = l.table().get(&key(PIPE)).unwrap();
    assert_eq!(wall.learned.class, Class::Wall, "{wall:?}");
    assert!(wall.learned.confidence >= 1, "{wall:?}");
    let air = l.table().get(&key(SKY)).unwrap();
    assert_eq!(air.learned.class, Class::Floor, "{air:?}");
}
