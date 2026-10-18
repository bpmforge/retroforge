//! Ticket W34-03 on real machines: the HeightLearner after a scripted 60 s of
//! play, scored against what the fixtures really contain.
//!
//! * RF-Scroller (NES): `level_data.c`'s own tables are the ground truth
//!   (collision bit 0 per metatile). The player only ever walks right, on the
//!   ground row, THROUGH the "solid" landmarks (the game never consults its
//!   collision table), so walked-through landmarks are scored as the misses
//!   they honestly are.
//! * RF-Scroller-S (SNES): the level is a pure function of the column (ground
//!   from row `24 - col % 4` down). It draws NO player sprite (W34-02's own
//!   test asserts it), so there is nothing for S1-S4 to watch: the number is
//!   reported, not fudged.
//! * SMW, SMB3, ALttP (`#[ignore]`, need the user's ROMs): no ground truth, so
//!   counts and an ASCII map of the final screen.
//!
//! ```text
//! cargo test --release -p retroforge --test w34_03_height_learner -- --ignored --nocapture
//! RF_SCREENS_ROMS_DIR=/path/to/Roms overrides the ROM folder.
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreEvent, CoreSink, EventMask, FrameBundleBuilder, InputFrame, PpuPixel};
use rf_enhance::canvas::{CanvasModel, FrameFeed};
use rf_enhance::heights::{
    cluster, Class, Dirs, Entity, FrameObs, HeightLearner, PlayerTracker, SpriteBox,
    TileHeightTable, TileKey,
};
use rf_enhance::scene_tracker::SceneTracker;
use rf_input::{NesButton, SnesButton};

const SIXTY_SECONDS: u32 = 3600;

struct Sink(FrameBundleBuilder);

impl CoreSink for Sink {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.0.video_scanline(y, pixels);
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, ev: CoreEvent) {
        self.0.event(ev);
    }
}

/// World (col, row) -> solid, `None` where the cell is not scored.
type Truth = Box<dyn Fn(i64, i64) -> Option<bool>>;

/// One scored cell of the world.
#[derive(Clone, Copy)]
struct Cell {
    key: TileKey,
    solid: bool,
}

struct Game {
    st: EmuStepper,
    sink: Sink,
    scene: SceneTracker,
    tracker: PlayerTracker,
    learner: HeightLearner,
    sprites: Vec<SpriteBox>,
    entities: Vec<Entity>,
    snes: bool,
    snes_cam: (i64, i64, Option<(u16, u16)>),
    size: (u16, u16),
    /// Learner cost per frame, microseconds.
    cost: Vec<f64>,
    /// Truth lookup: world (col, row) -> solid; set by the fixture tests.
    truth: Option<Truth>,
    /// Filter: which frames' grids are scored (NES: the plain playfield).
    nes: bool,
    unwrap_x: Option<(i64, i64)>,
    visited: BTreeMap<(i64, i64), Cell>,
    /// Frames the tracker saw a player on.
    with_player: u32,
    /// Frames the tracker held a name (seen or not).
    with_tracked: u32,
    /// The canvas model, fed as the shell feeds it: its HUD verdict per layer.
    canvas: CanvasModel,
    canvas_scene: u64,
    canvas_prev: Option<[Option<rf_enhance::canvas::LayerScroll>; 4]>,
    /// Frames some layer was flagged HUD.
    hud_frames: u32,
    /// Bus addresses of the player's X and Y words (a profile's `[[scout.addr]]`).
    hint_addrs: Option<(u32, u32)>,
    was_named: bool,
}

impl Game {
    fn open(raw: &[u8]) -> Self {
        let mut st = EmuStepper::open(raw).expect("opens");
        st.set_event_mask(EventMask::ALL);
        st.set_tile_capture(true);
        let snes = st.bg_layer_views().is_some();
        let size = if snes { (256, 224) } else { (256, 240) };
        let mut bundle = FrameBundleBuilder::new(size.0, size.1);
        bundle.set_capture_wide(true);
        Game {
            st,
            sink: Sink(bundle),
            scene: SceneTracker::new(),
            tracker: PlayerTracker::new(),
            learner: HeightLearner::new(),
            sprites: Vec::new(),
            entities: Vec::new(),
            snes,
            snes_cam: (0, 0, None),
            size,
            cost: Vec::new(),
            truth: None,
            nes: !snes,
            unwrap_x: None,
            visited: BTreeMap::new(),
            with_player: 0,
            with_tracked: 0,
            canvas: CanvasModel::with_cache("w34".into(), None),
            canvas_scene: 1,
            canvas_prev: None,
            hud_frames: 0,
            hint_addrs: None,
            was_named: false,
        }
    }

    fn bit(&self, snes: SnesButton, nes: NesButton) -> u16 {
        1 << if self.snes { snes.bit() } else { nes.bit() }
    }

    fn dirs(&self, mask: u16) -> Dirs {
        let has = |s, n| mask & self.bit(s, n) != 0;
        Dirs {
            left: has(SnesButton::Left, NesButton::Left),
            right: has(SnesButton::Right, NesButton::Right),
            up: has(SnesButton::Up, NesButton::Up),
            down: has(SnesButton::Down, NesButton::Down),
        }
    }

    fn camera(&mut self) -> Option<(i64, i64)> {
        if !self.snes {
            return self.scene.last_world_position();
        }
        let v = self.st.bg_layer_views()?[0];
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

    /// One frame; `watch` feeds the tracker and the learner as well.
    fn frame(&mut self, mask: u16, watch: bool) {
        let mut f = InputFrame::empty();
        f.ports[0] = mask;
        self.st.latch_and_advance_frame(f, &mut self.sink);
        let bundle = self.sink.0.take(self.st.frame_count());
        self.scene.observe_frame(
            &bundle.video,
            bundle.width,
            bundle.height,
            &bundle.events,
            &[],
        );
        if !watch {
            return;
        }
        let camera = self.camera();
        let pad = self.dirs(mask);
        // The HUD verdict is the canvas model's, as the shell reads it.
        if self.snes {
            let now = retroforge::core_thread::snes_layer_scrolls(&self.st);
            let drawn_with = std::mem::replace(&mut self.canvas_prev, now);
            self.canvas.observe_frame_in(
                &bundle,
                rf_enhance::scene_identity::SceneId(1),
                &FrameFeed::registers(drawn_with.as_ref()),
            );
            self.canvas_scene = 1;
        } else {
            self.canvas_scene = self.canvas.observe_frame(&bundle, &FrameFeed::bands()).0;
        }
        let hud: [bool; 4] = self.canvas.hud_layers(self.canvas_scene);
        self.hud_frames += u32::from(hud.iter().any(|h| *h));
        let grids = self
            .st
            .tile_grids(&bundle.lines, hud, usize::from(self.size.1));
        self.st.sprite_boxes(
            (i32::from(self.size.0), i32::from(self.size.1)),
            &mut self.sprites,
        );
        cluster(&self.sprites, &mut self.entities);
        let word =
            |g: &Game, a: u32| i64::from(g.st.peek_bus(a)) | i64::from(g.st.peek_bus(a + 1)) << 8;
        let (hint_x, hint_y) = self.hint_addrs.map_or((None, None), |(ax, ay)| {
            (Some(word(self, ax)), Some(word(self, ay)))
        });
        self.tracker.observe(&rf_enhance::heights::FrameInput {
            sprites: &self.sprites,
            pad,
            camera,
            hint_x,
            hint_y,
        });
        self.trace(pad, camera);
        let orientation = self.tracker.orientation().orientation;
        self.with_player += u32::from(self.tracker.player().is_some());
        self.with_tracked += u32::from(self.tracker.tracked().is_some());
        let t = Instant::now();
        self.learner.observe(&FrameObs {
            grids: &grids,
            sprites: &self.sprites,
            entities: &self.entities,
            player: self.tracker.player(),
            pad,
            orientation,
            hidden: None,
        });
        self.cost.push(t.elapsed().as_secs_f64() * 1e6);
        if self.truth.is_some() {
            self.record(&grids);
        }
    }

    /// `RF_TRACE=1`: every 10th frame and every seen/unseen change, one line
    /// of tracker state; `RF_TRACE_SPR=f1,f2` also lists the sprites on those
    /// frames. A debugging aid for the recall numbers, silent otherwise.
    fn trace(&mut self, pad: Dirs, camera: Option<(i64, i64)>) {
        if std::env::var("RF_TRACE").is_err() {
            return;
        }
        let seen = self.tracker.player().is_some();
        let k = self.cost.len();
        if k.is_multiple_of(10) || seen != self.was_named {
            let held = self
                .tracker
                .tracked()
                .map(|p| (p.track, p.visible, p.entity.x0, p.entity.y0));
            println!(
                "TR f{k} votes{:?} tracked{held:?} pad{pad:?} cam{camera:?} seen{seen} ents{} tracks{}",
                self.tracker.vertical_votes(),
                self.entities.len(),
                self.tracker.track_evidence().count()
            );
        }
        self.was_named = seen;
        if let Ok(list) = std::env::var("RF_TRACE_SPR") {
            if list.split(',').any(|v| v.parse::<usize>().ok() == Some(k)) {
                let v: Vec<_> = self
                    .sprites
                    .iter()
                    .map(|s| (s.x, s.y, s.w, s.h, s.tile))
                    .collect();
                println!("SPR f{k} {v:?}");
            }
        }
    }

    /// Note the key at every world cell this frame shows.
    fn record(&mut self, grids: &[rf_enhance::heights::TileGrid]) {
        let truth = self.truth.as_ref().expect("truth");
        // NES: the plain playfield band only (the HUD band and the tail's
        // vertical sub-area are skipped). SNES: the one grid.
        let g = if self.nes {
            grids
                .iter()
                .find(|g| g.rows_px.0 == 16 && g.origin_world.1 == 16)
                .filter(|_| grids.len() == 2)
        } else {
            grids.first()
        };
        let Some(g) = g else {
            self.unwrap_x = None;
            return;
        };
        let period = if self.nes { 512 } else { 1024 };
        let ux = match self.unwrap_x {
            None => g.origin_world.0,
            Some((last_raw, ux)) => {
                let mut d = g.origin_world.0 - last_raw;
                if d > period / 2 {
                    d -= period;
                } else if d < -period / 2 {
                    d += period;
                }
                ux + d
            }
        };
        self.unwrap_x = Some((g.origin_world.0, ux));
        let p = i64::from(g.cell_px);
        for row in 0..g.rows {
            for col in 0..g.cols {
                let (Some(key), Some(cell)) = (g.key_at(col, row), g.cell(col, row)) else {
                    continue;
                };
                if cell.partial() {
                    continue;
                }
                let wc = (ux + i64::from(col) * p).div_euclid(p);
                let wr = (g.origin_world.1 + i64::from(row) * p).div_euclid(p);
                if let Some(solid) = truth(wc, wr) {
                    self.visited.insert((wc, wr), Cell { key, solid });
                }
            }
        }
    }

    fn run(&mut self, mask: u16, n: u32, watch: bool) {
        for _ in 0..n {
            self.frame(mask, watch);
        }
    }

    fn tap(&mut self, mask: u16, hold: u32) {
        self.run(mask, hold, false);
        self.run(0, 8, false);
    }

    /// `wN` idle N frames, else `<buttons>N`: `A B X Y S E U D L R` joined by `+`.
    fn script(&mut self, text: &str, watch: bool) {
        for tok in text.split_whitespace() {
            let (mask, n) = if let Some(n) = tok.strip_prefix('w') {
                (0, n.parse::<u32>().expect("wN"))
            } else {
                let at = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
                (self.mask_of(&tok[..at]), tok[at..].parse().expect("N"))
            };
            self.run(mask, n, watch);
        }
    }

    fn mask_of(&self, buttons: &str) -> u16 {
        use NesButton as N;
        use SnesButton as S;
        buttons.split('+').fold(0, |m, c| {
            m | match c {
                "A" => self.bit(S::A, N::A),
                "B" => self.bit(S::B, N::B),
                "X" if self.snes => self.bit(S::X, N::A),
                "Y" if self.snes => self.bit(S::Y, N::A),
                "S" => self.bit(S::Start, N::Start),
                "E" => self.bit(S::Select, N::Select),
                "U" => self.bit(S::Up, N::Up),
                "D" => self.bit(S::Down, N::Down),
                "L" => self.bit(S::Left, N::Left),
                "R" => self.bit(S::Right, N::Right),
                _ => 0,
            }
        })
    }

    /// Fresh tracker and learner: the scripted play begins now.
    fn begin(&mut self) {
        self.tracker = PlayerTracker::new();
        self.learner = HeightLearner::new();
        self.cost.clear();
        self.visited.clear();
        self.unwrap_x = None;
        self.with_player = 0;
        self.with_tracked = 0;
        self.hud_frames = 0;
    }

    fn score(&self) -> Score {
        let t = self.learner.table();
        let mut s = Score::default();
        let mut keys: BTreeMap<TileKey, bool> = BTreeMap::new();
        for c in self.visited.values() {
            let learned = t
                .get(&c.key)
                .is_some_and(|e| e.learned.drawn().0.is_solid());
            match (learned, c.solid) {
                (true, true) => s.tp += 1,
                (true, false) => s.fp += 1,
                (false, true) => s.fneg += 1,
                (false, false) => s.tn += 1,
            }
            keys.insert(c.key, c.solid);
        }
        for (k, solid) in keys {
            let learned = t.get(&k).is_some_and(|e| e.learned.drawn().0.is_solid());
            match (learned, solid) {
                (true, true) => s.key_tp += 1,
                (true, false) => s.key_fp += 1,
                (false, true) => s.key_fn += 1,
                _ => {}
            }
        }
        s
    }

    fn report_cost(&self, label: &str) {
        let mut c = self.cost.clone();
        if c.is_empty() {
            return;
        }
        c.sort_by(f64::total_cmp);
        println!(
            "COST {label}: learner.observe median {:.1} us, p95 {:.1} us, max {:.1} us over {} frames",
            c[c.len() / 2],
            c[(c.len() - 1) * 95 / 100],
            c[c.len() - 1],
            c.len()
        );
    }
}

#[derive(Default, Debug)]
struct Score {
    tp: u32,
    fp: u32,
    fneg: u32,
    tn: u32,
    key_tp: u32,
    key_fp: u32,
    key_fn: u32,
}

impl Score {
    fn precision(&self) -> Option<f32> {
        (self.tp + self.fp > 0).then(|| self.tp as f32 / (self.tp + self.fp) as f32)
    }
    fn recall(&self) -> Option<f32> {
        (self.tp + self.fneg > 0).then(|| self.tp as f32 / (self.tp + self.fneg) as f32)
    }
    fn show(&self, label: &str, g: &Game) {
        println!(
            "ORACLE {label}: precision {:?}, recall {:?} over {} visited cells \
             (tp {} fp {} fn {} tn {}); by key: tp {} fp {} fn {}; learner {:?}",
            self.precision(),
            self.recall(),
            g.visited.len(),
            self.tp,
            self.fp,
            self.fneg,
            self.tn,
            self.key_tp,
            self.key_fp,
            self.key_fn,
            g.learner.stats()
        );
    }
}

fn rom(console: &str, file: &str) -> Option<Vec<u8>> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p: PathBuf = Path::new(&root).join(console).join(file);
    if !p.exists() {
        println!("SKIP: {} not found", p.display());
        return None;
    }
    Some(retroforge::rom_open::load_rom_bytes(&p).expect("rom"))
}

fn fixture(rel: &str) -> Option<Vec<u8>> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel);
    let bytes = std::fs::read(&p).ok();
    if bytes.is_none() {
        assert!(
            std::env::var_os("CI").is_none(),
            "fixture {rel} missing on CI"
        );
        println!("SKIP: fixture {rel} not built");
    }
    bytes
}

/// The numbers in the `{ ... }` block that follows `name` in a C source,
/// comments removed.
fn c_array(src: &str, name: &str) -> Vec<u32> {
    let mut clean = String::new();
    let mut rest = src;
    while let Some(i) = rest.find("/*") {
        clean.push_str(&rest[..i]);
        rest = &rest[i + 2..];
        rest = rest.find("*/").map_or("", |j| &rest[j + 2..]);
    }
    clean.push_str(rest);
    let at = clean.find(name).expect("array");
    let body = &clean[clean[at..].find('{').expect("{") + at + 1..];
    let body = &body[..body.find('}').expect("}")];
    body.split(',')
        .filter_map(|t| {
            let t = t.trim();
            t.strip_prefix("0x")
                .map(|h| u32::from_str_radix(h, 16).expect("hex"))
                .or_else(|| t.parse().ok())
        })
        .collect()
}

/// RF-Scroller's level, decoded from its own source tables.
fn nes_truth() -> Truth {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/nes/rf-scroller/src/level_data.c"),
    )
    .expect("level_data.c");
    let offsets = c_array(&src, "level_column_offset");
    let rle = c_array(&src, "level_rle_data");
    let collision = c_array(&src, "collision_table");
    let mut cols: Vec<Vec<bool>> = Vec::new();
    for (i, &off) in offsets.iter().enumerate() {
        let end = offsets.get(i + 1).map_or(rle.len(), |&e| e as usize);
        let mut col = Vec::new();
        for pair in rle[off as usize..end].chunks(2) {
            for _ in 0..pair[0] {
                col.push(collision[pair[1] as usize] & 1 == 1);
            }
        }
        assert_eq!(col.len(), 14, "column {i} sums to 14 rows");
        cols.push(col);
    }
    // Columns 44-47 are the vertical sub-area's ladder: not scored.
    Box::new(move |c, r| {
        // Playfield row 0 sits under the 16 px HUD: nametable row 1.
        let lr = r - 1;
        (0..44).contains(&c).then_some(())?;
        (0..14).contains(&lr).then_some(())?;
        Some(cols[c as usize][lr as usize])
    })
}

/// RF-Scroller-S: ground from row `24 - col % 4` down to row 27.
fn snes_truth() -> Truth {
    Box::new(|c, r| (c >= 0 && (0..28).contains(&r)).then(|| r >= 24 - c.rem_euclid(4)))
}

fn boot_fixture(raw: &[u8]) -> Game {
    let mut g = Game::open(raw);
    g.run(0, 120, false);
    g.tap(g.mask_of("S"), 4);
    g.run(0, 60, false);
    g
}

/// The NES oracle over `script` (>= `min_frames` watched frames).
fn nes_oracle(label: &str, script: &str, min_frames: usize) {
    let Some(raw) = fixture("nes/rf-scroller/build/rf-scroller.nes") else {
        return;
    };
    let mut g = boot_fixture(&raw);
    g.truth = Some(nes_truth());
    g.begin();
    g.script(script, true);
    assert!(g.cost.len() >= min_frames);
    let s = g.score();
    s.show(label, &g);
    g.report_cost(label);
    println!(
        "PLAYER {label}: {:?}",
        g.tracker.first_found().map(|p| p.entity)
    );
    let (p, r) = (
        s.precision().expect("learned some"),
        s.recall().expect("had solids"),
    );
    assert!(p >= 0.90, "precision {p}");
    assert!(r >= 0.70, "recall {r}");
    // Unknown tiles are flat: nothing the learner is unsure of is extruded.
    for (_, e) in g.learner.table().iter() {
        let (c, d) = e.learned.drawn();
        if e.learned.confidence < 2 {
            assert_eq!((c, d), (Class::Unknown, 0));
        }
    }
}

/// The level is 704 px long at one pixel a frame: 740 frames walk all of it.
#[test]
fn rf_scroller_nes_walks_the_level() {
    nes_oracle("rf-scroller (12 s)", "w4 R740", 740);
}

/// The plan's 60 s (release: `-- --ignored`).
#[test]
#[ignore = "3600 frames; run deliberately in release"]
fn rf_scroller_nes_after_sixty_seconds() {
    nes_oracle(
        "rf-scroller (60 s)",
        "w4 R900 w30 U60 D60 w10 R2536",
        SIXTY_SECONDS as usize,
    );
}

fn snes_oracle(label: &str, script: &str, min_frames: usize) {
    let Some(raw) = fixture("snes/rf-scroller-s/build/rf-scroller-s.sfc") else {
        return;
    };
    let mut g = boot_fixture(&raw);
    g.truth = Some(snes_truth());
    g.begin();
    g.script(script, true);
    assert!(g.cost.len() >= min_frames);
    let s = g.score();
    s.show(label, &g);
    g.report_cost(label);
    // No sprite answers the pad (W34-02 asserts the same), so nothing can be
    // learned from play: the honest result is "flat", never a guess.
    assert!(g.tracker.player().is_none());
    assert_eq!(s.fp, 0, "it must not invent solids");
    assert_eq!(g.learner.table().drawn_count_solid(), 0);
}

#[test]
fn rf_scroller_s_has_no_player_to_learn_from() {
    snes_oracle("rf-scroller-s (5 s)", "w4 R296", 300);
}

#[test]
#[ignore = "3600 frames; run deliberately in release"]
fn rf_scroller_s_after_sixty_seconds() {
    snes_oracle("rf-scroller-s (60 s)", "w4 R3596", SIXTY_SECONDS as usize);
}

#[test]
fn the_same_play_gives_the_same_table_and_the_cache_round_trips_it() {
    let Some(raw) = fixture("nes/rf-scroller/build/rf-scroller.nes") else {
        return;
    };
    let tables: Vec<TileHeightTable> = (0..2)
        .map(|_| {
            let mut g = boot_fixture(&raw);
            g.begin();
            g.script("w4 R400", true);
            g.learner.table().clone()
        })
        .collect();
    assert_eq!(tables[0], tables[1]);
    assert_eq!(tables[0].to_bytes(), tables[1].to_bytes());
    assert!(!tables[0].is_empty());
    let root = std::env::temp_dir().join(format!("rf-w34-03-{}", std::process::id()));
    let mut cache = rf_cache::Cache::open(&root, 8 << 20).unwrap();
    rf_enhance::heights::save_table(&mut cache, "fixture-rom", &tables[0]).unwrap();
    let back = rf_enhance::heights::load_table(&mut cache, "fixture-rom")
        .unwrap()
        .expect("hit");
    assert_eq!(back, tables[0]);
    let _ = std::fs::remove_dir_all(root);
}

// ---- real games: counts and a map, no ground truth ----

const PLATFORM_PLAY: &str = "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20";

fn real_report(label: &str, g: &Game) {
    let s = g.learner.stats();
    let t = g.learner.table();
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    for (_, e) in t.iter() {
        let (c, _) = e.learned.drawn();
        *by.entry(format!("{c:?}")).or_default() += 1;
    }
    println!(
        "REAL {label}: {s:?}; classes drawn {by:?}; orientation {:?}",
        g.tracker.orientation()
    );
    println!(
        "REAL {label}: player {:?}",
        g.tracker.first_found().map(|p| (p.found_at, p.source))
    );
    g.report_cost(label);
    println!(
        "REAL {label}: player on screen {} / named (held through gaps) {} of {} frames; HUD flagged on {} frames",
        g.with_player,
        g.with_tracked,
        g.cost.len(),
        g.hud_frames
    );
    let mut top: Vec<_> = t.iter().collect();
    top.sort_by_key(|(_, e)| {
        std::cmp::Reverse(
            e.evidence.floor_top + e.evidence.blocked.iter().sum::<u32>() + e.evidence.mass,
        )
    });
    for (k, e) in top.into_iter().take(10) {
        println!(
            "  TOP layer {} {:016x}/{:04x} {:?} {:?}",
            k.layer, k.pattern, k.palette, e.evidence, e.learned
        );
    }
}

fn ascii_map(label: &str, g: &mut Game) {
    // One more frame to look at the current screen.
    let mut f = InputFrame::empty();
    f.ports[0] = 0;
    g.st.latch_and_advance_frame(f, &mut g.sink);
    let bundle = g.sink.0.take(g.st.frame_count());
    let hud: [bool; 4] = g.canvas.hud_layers(g.canvas_scene);
    let grids = g.st.tile_grids(&bundle.lines, hud, usize::from(g.size.1));
    for grid in grids.iter().filter(|x| !x.hud) {
        let hg = g.learner.grid_for(grid);
        let mut solid = 0;
        let mut drawn = 0;
        let mut out = String::new();
        for r in 0..hg.rows {
            for c in 0..hg.cols {
                let i = usize::from(r) * usize::from(hg.cols) + usize::from(c);
                let ch = match hg.class[i] {
                    Class::Unknown => '?',
                    Class::Floor => '.',
                    Class::Wall => 'W',
                    Class::Tall => 'T',
                    Class::Platform => 'P',
                    Class::Ramp(_) => '/',
                    Class::Backdrop => 'b',
                };
                if ch != '?' {
                    drawn += 1;
                }
                if hg.class[i].is_solid() {
                    solid += 1;
                }
                out.push(ch);
            }
            out.push('\n');
        }
        println!(
            "MAP {label} layer {} rows {:?}: {drawn}/{} cells drawn, {solid} solid\n{out}",
            grid.layer,
            grid.rows_px,
            hg.class.len()
        );
    }
}

fn platformer(label: &str, g: &mut Game) {
    g.begin();
    let mut mapped = false;
    while (g.cost.len() as u32) < SIXTY_SECONDS {
        g.script(PLATFORM_PLAY, true);
        println!(
            "ITER {label} frame {}: {:?} player {:?}",
            g.cost.len(),
            g.learner.stats(),
            g.tracker.player().map(|p| p.entity)
        );
        if !mapped && g.cost.len() >= 900 {
            mapped = true;
            ascii_map(&format!("{label} @{}", g.cost.len()), g);
        }
    }
    real_report(label, g);
    ascii_map(label, g);
}

/// SMW up to the first level, optionally with the profile's player X/Y words.
fn smw_game(hinted: bool) -> Option<Game> {
    let raw = rom("snes", "Super Mario World (USA).zip")?;
    let mut g = Game::open(&raw);
    if hinted {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles/snes/super-mario-world/profile.toml");
        let profile = rf_profiles::load_file(&path)
            .expect("profile loads")
            .profile;
        let rows = &profile.scout.as_ref().expect("[scout]").addr;
        let find = |role: &str| rows.iter().find(|r| r.role == role).expect(role).addr;
        g.hint_addrs = Some((find("player_x"), find("player_y")));
    }
    g.run(0, 200, false);
    for i in 0..10 {
        g.tap(g.mask_of(if i % 2 == 0 { "S" } else { "A" }), 6);
        g.run(0, 100, false);
    }
    g.run(0, 300, false);
    g.tap(g.mask_of("A"), 8);
    g.run(0, 400, false);
    g.tap(g.mask_of("L"), 8);
    g.run(0, 150, false);
    g.tap(g.mask_of("A"), 8);
    g.run(0, 150, false);
    Some(g)
}

#[test]
#[ignore = "real ROM; run deliberately in release"]
fn smw() {
    let Some(mut g) = smw_game(false) else { return };
    platformer("smw", &mut g);
}

#[test]
#[ignore = "real ROM; run deliberately in release"]
fn smw_with_profile_hints() {
    let Some(mut g) = smw_game(true) else { return };
    platformer("smw-hinted", &mut g);
}

#[test]
#[ignore = "real ROM; run deliberately in release"]
fn smb3() {
    let Some(raw) = rom("nes", "Super Mario Bros. 3 (USA).zip") else {
        return;
    };
    let mut g = Game::open(&raw);
    g.run(0, 300, false);
    g.tap(g.mask_of("S"), 6);
    g.run(0, 120, false);
    g.tap(g.mask_of("S"), 6);
    g.run(0, 400, false);
    g.tap(g.mask_of("R"), 8);
    g.run(0, 60, false);
    g.tap(g.mask_of("U"), 8);
    g.run(0, 60, false);
    g.tap(g.mask_of("A"), 8);
    g.run(0, 100, false);
    platformer("smb3", &mut g);
}

#[test]
#[ignore = "real ROM; run deliberately in release"]
fn alttp() {
    let Some(raw) = rom(
        "snes",
        "Legend of Zelda, The - A Link to the Past (USA).zip",
    ) else {
        return;
    };
    let mut g = Game::open(&raw);
    g.run(0, 600, false);
    for i in 0..30 {
        g.tap(g.mask_of(if i % 2 == 0 { "S" } else { "A" }), 6);
        g.run(0, 60, false);
    }
    for _ in 0..24 {
        g.tap(g.mask_of("A"), 4);
        g.run(0, 50, false);
    }
    g.begin();
    while (g.cost.len() as u32) < SIXTY_SECONDS {
        g.script("w4 D30 w15 R40 w15 U20 w15 L40 w15 D20 w20", true);
    }
    real_report("alttp", &g);
    ascii_map("alttp", &mut g);
}
