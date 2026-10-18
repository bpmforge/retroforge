//! Shared by `w33_07_discovery.rs` (the acceptance tests) and `w30_screens.rs`
//! (the Mega Man X shot): a game under a scripted play session, fed the way the
//! core thread feeds the canvas model and the scout-settings finder; and the
//! driver that finds a table and scouts with it. Not a test target itself
//! (cargo only builds `tests/*.rs` and `tests/*/main.rs`).

use std::path::{Path, PathBuf};

use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreEvent, CoreSink, FrameBundleBuilder, InputFrame, PpuPixel};
use rf_enhance::canvas::{scrolls_for, CanvasModel, FrameFeed, FrameScrolls, ScrollLag};
use rf_enhance::scene_tracker::{SceneHints, SETTLING};
use rf_enhance::scout_finder::{Frame, Observed, Pad, ScoutFinder};
use rf_input::{NesButton, SnesButton};

pub struct Sink {
    pub bundle: FrameBundleBuilder,
    /// The picture as the player sees it (screenshots of an explored session).
    pub fb: rf_renderer::FrameBuffer,
}

impl Sink {
    pub fn new(w: u16, h: u16) -> Self {
        Self {
            bundle: FrameBundleBuilder::new(w, h),
            fb: rf_renderer::FrameBuffer::with_size(usize::from(w), usize::from(h)),
        }
    }
}

impl CoreSink for Sink {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.bundle.video_scanline(y, pixels);
        self.fb.video_scanline(y, pixels);
    }
    fn sub_scanline(&mut self, y: u16, pixels: &[rf_core_api::SubPixel], fixed_color: u16) {
        self.fb.sub_scanline(y, pixels, fixed_color);
    }
    fn palette_scanline(&mut self, y: u16, palette: &[u16], brightness: u8) {
        self.fb.palette_scanline(y, palette, brightness);
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, ev: CoreEvent) {
        self.bundle.event(ev);
    }
}

/// One game under a scripted session: the machine, the live canvas model, the
/// finder, fed the way the core thread feeds them.
pub struct Rig {
    pub st: EmuStepper,
    pub sink: Sink,
    pub live: CanvasModel,
    pub finder: ScoutFinder,
    pub snes: bool,
    pub scene_key: Option<retroforge::scene_key::SceneKeySpec>,
    lag: ScrollLag,
    pub last_scene: Option<u64>,
    /// The ROM's bytes (the trial machines open their own).
    pub rom: Vec<u8>,
    /// The core thread's half of "Find scout settings", fed the same frames.
    pub find: Option<retroforge::scout_find::ScoutFind>,
    /// What it reported.
    pub find_out: Vec<retroforge::scout_find::Out>,
}

pub fn roms_dir() -> String {
    std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string())
}

pub fn rom_file(console: &str, file: &str) -> Option<PathBuf> {
    let p = Path::new(&roms_dir()).join(console).join(file);
    if p.exists() {
        Some(p)
    } else {
        println!("SKIP: {file} not found");
        None
    }
}

impl Rig {
    /// `profile_rel`: the profile whose `[scene]` table feeds the scene tracker
    /// (as the app does); its `[scout]` table is NOT read.
    pub fn open(console: &str, file: &str, profile_rel: Option<&str>) -> Option<Self> {
        let path = rom_file(console, file)?;
        let rom = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
        let mut st = EmuStepper::open(&rom).expect("opens");
        st.set_event_mask(rf_core_api::EventMask::ALL);
        let snes = console == "snes";
        let scene_key = profile_rel.and_then(|rel| {
            let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
            let outcome = rf_profiles::load_file(&p).expect("profile");
            outcome
                .profile
                .scene
                .as_ref()
                .map(retroforge::scene_key::SceneKeySpec::from_profile)
        });
        let (w, h) = if snes { (256, 224) } else { (256, 240) };
        let len = if snes { 0x2000 } else { 0x800 + 0x2000 };
        Some(Self {
            st,
            sink: Sink::new(w, h),
            live: CanvasModel::new(),
            finder: ScoutFinder::new(len, if snes { 2 } else { 1 }),
            snes,
            scene_key,
            lag: ScrollLag::default(),
            last_scene: None,
            rom,
            find: None,
            find_out: Vec::new(),
        })
    }

    /// Run one frame with the pad `mask` (NES bits or SNES bits), feeding the
    /// live model and the finder.
    pub fn frame(&mut self, mask: u16) {
        let mut input = InputFrame::empty();
        input.ports[0] = mask;
        self.st.latch_and_advance_frame(input, &mut self.sink);
        let bundle = self.sink.bundle.take(self.st.frame_count());
        let level = self
            .scene_key
            .as_ref()
            .and_then(|k| k.read(|a| self.st.peek_bus(a)));
        let hints = SceneHints {
            level: level.as_deref(),
            axis_sections: self.scene_key.as_ref().and_then(|k| k.axis_sections),
        };
        let (scene, scroll): (_, Vec<(Vec<u8>, Vec<u8>)>);
        if self.snes {
            let views = self.st.bg_layer_views().expect("snes");
            let read = FrameScrolls {
                scene: Some((views[0].hofs, views[0].vofs)),
                layers: retroforge::core_thread::snes_layer_scrolls(&self.st),
            };
            let drawn = self.lag.drawn_with(self.st.frame_count(), read);
            let feed = FrameFeed::registers(scrolls_for(&bundle, &drawn, &read))
                .with_scene_scroll(drawn.scene)
                .with_scene_hints(hints);
            scene = self.live.observe_frame(&bundle, &feed);
            scroll = (0..2)
                .map(|i| {
                    (
                        vec![(views[i].hofs & 0xFF) as u8],
                        vec![(views[i].vofs & 0xFF) as u8],
                    )
                })
                .collect();
        } else {
            scene = self
                .live
                .observe_frame(&bundle, &FrameFeed::bands().with_scene_hints(hints));
            let (mut xs, mut ys) = (Vec::new(), Vec::new());
            for e in &bundle.events {
                if let CoreEvent::ScrollWrite { x, y, .. } = e {
                    xs.push((x & 0xFF) as u8);
                    ys.push((y & 0xFF) as u8);
                }
            }
            scroll = vec![(xs, ys)];
        }
        let scene_id = scene;
        let scene = (scene != SETTLING).then_some(scene.0);
        self.last_scene = scene;
        if let Some(find) = self.find.as_mut() {
            let outs = find.frame(&retroforge::scout_find::Fed {
                stepper: &self.st,
                bundle: &bundle,
                pad: mask,
                scene: scene_id,
                rom: &self.rom,
                canvases: &self.live,
                size: (bundle.width, bundle.height),
                scene_key: self.scene_key.as_ref(),
            });
            for o in &outs {
                if let retroforge::scout_find::Out::Progress(p) = o {
                    println!("FIND-PROGRESS at frame {}: {p:?}", self.st.frame_count());
                }
            }
            self.find_out.extend(outs);
        }
        let (left, right, face_bits) = if self.snes {
            (
                1u16 << SnesButton::Left.bit(),
                1 << SnesButton::Right.bit(),
                [
                    Some(SnesButton::B.bit()),
                    Some(SnesButton::A.bit()),
                    Some(SnesButton::X.bit()),
                    Some(SnesButton::Y.bit()),
                ],
            )
        } else {
            (
                1 << NesButton::Left.bit(),
                1 << NesButton::Right.bit(),
                [
                    Some(NesButton::A.bit()),
                    Some(NesButton::B.bit()),
                    None,
                    None,
                ],
            )
        };
        let face = face_bits
            .iter()
            .enumerate()
            .filter(|(_, b)| b.is_some_and(|b| mask & (1 << b) != 0))
            .fold(0u8, |m, (i, _)| m | (1 << i));
        let pad = if mask & right != 0 {
            Pad::Right
        } else if mask & left != 0 {
            Pad::Left
        } else {
            Pad::Idle
        };
        let nes_ram: Vec<u8>;
        let ram: &[u8] = match self.st.snes_work_ram_head(0x2000) {
            Some(r) => r,
            None => {
                nes_ram = [&self.st.wram_snapshot()[..], &self.st.prg_ram()[..]].concat();
                &nes_ram
            }
        };
        let layers: Vec<(&[u8], &[u8])> = scroll
            .iter()
            .map(|(x, y)| (x.as_slice(), y.as_slice()))
            .collect();
        self.finder.observe(&Frame {
            ram,
            pad,
            jump: face,
            scroll: &layers,
            scene,
        });
    }

    /// Start the live model, the scroll lag and the finder afresh (a machine
    /// restored from a snapshot is a new session).
    pub fn reset(&mut self) {
        let len = if self.snes { 0x2000 } else { 0x800 + 0x2000 };
        self.live = CanvasModel::new();
        self.finder = ScoutFinder::new(len, if self.snes { 2 } else { 1 });
        self.lag = ScrollLag::default();
        self.last_scene = None;
    }

    /// Save the current picture.
    pub fn shot(&self, path: &Path) {
        image::save_buffer(
            path,
            self.sink.fb.rgba(),
            self.sink.fb.width() as u32,
            self.sink.fb.height() as u32,
            image::ColorType::Rgba8,
        )
        .expect("png");
        println!("SHOT {}", path.display());
    }

    /// `wN` idle N frames, `<buttons>N` hold N frames. Buttons: `A B X Y`
    /// face, `S` start, `E` select, `U D L R`, joined by `+`.
    pub fn script(&mut self, text: &str) {
        for tok in text.split_whitespace() {
            let (mask, n) = if let Some(n) = tok.strip_prefix('w') {
                (0, n.parse::<u32>().expect("wN"))
            } else {
                let split = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
                let (b, n) = tok.split_at(split);
                (self.mask_of(b), n.parse().expect("N"))
            };
            for _ in 0..n {
                self.frame(mask);
            }
        }
    }

    fn mask_of(&self, tok: &str) -> u16 {
        tok.split('+').fold(0, |m, c| {
            let bit = if self.snes {
                match c {
                    "A" => SnesButton::A.bit(),
                    "B" => SnesButton::B.bit(),
                    "X" => SnesButton::X.bit(),
                    "Y" => SnesButton::Y.bit(),
                    "S" => SnesButton::Start.bit(),
                    "E" => SnesButton::Select.bit(),
                    "U" => SnesButton::Up.bit(),
                    "D" => SnesButton::Down.bit(),
                    "L" => SnesButton::Left.bit(),
                    "R" => SnesButton::Right.bit(),
                    _ => return m,
                }
            } else {
                match c {
                    "A" => NesButton::A.bit(),
                    "B" => NesButton::B.bit(),
                    "E" => NesButton::Select.bit(),
                    "S" => NesButton::Start.bit(),
                    "U" => NesButton::Up.bit(),
                    "D" => NesButton::Down.bit(),
                    "L" => NesButton::Left.bit(),
                    "R" => NesButton::Right.bit(),
                    _ => return m,
                }
            };
            m | (1 << bit)
        })
    }
}

pub const SMW: (&str, &str, &str) = (
    "snes",
    "Super Mario World (USA).zip",
    "../../profiles/snes/super-mario-world/profile.toml",
);
pub const SMB3: (&str, &str, &str) = (
    "nes",
    "Super Mario Bros. 3 (USA).zip",
    "../../profiles/nes/super-mario-bros-3/profile.toml",
);
pub const MM2: (&str, &str, &str) = (
    "nes",
    "Mega Man 2 (USA).zip",
    "../../profiles/nes/mega-man-2/profile.toml",
);
pub const MMX: (&str, &str, &str) = (
    "snes",
    "Mega Man X (USA).zip",
    "../../profiles/snes/mega-man-x/profile.toml",
);

/// Boot, menus, the map at Yoshi's House, left to Yoshi's Island 1, enter, and
/// wait for the fade-in (`w32_07_scout_smw::reach_yi1` as tokens).
pub const SMW_TO_LEVEL: &str = "w200 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 w300 A8 w408 L8 w158 A8 w158";
pub const MMX_TO_LEVEL: &str = "w300 S6 w114 S6 w114 S6 w114 S6 w114 S6 w400";
pub const SMB3_TO_LEVEL: &str = "w300 S6 w120 S6 w400 R8 w60 U8 w60 A8 w300";
pub const MM2_TO_LEVEL: &str = "w400 S6 w120 S6 w300 S6 w300 U8 w20 S6 w800";

/// What the player does once in the level: stand, jump four times (standing),
/// walk into the left wall, then right a long way (jumping now and again).
pub const SMW_PLAY: &str = "L2 w3 B8 w52 B8 w52 B8 w52 B8 w52 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22";
pub const SMB3_PLAY: &str = "L30 w20 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 w20 A8 w60 A8 w60 A8 w60 A8 w60 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24";
/// Mega Man X: the same, with B to jump.
pub const MMX_PLAY: &str = "L30 w5 R40 R+B20 R40 R+B20 R40 R+B20 R40 R+B20 w20 B8 w60 B8 w60 B8 w60 B8 w60 R40 R+B20 R40 R+B20 R40 R+B20 R40 R+B20";
pub const MM2_PLAY: &str = "L30 w5 A8 w60 A8 w60 A8 w60 A8 w60 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 w400 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15";

pub fn describe(o: &Observed) {
    println!("camera: {:?}", o.camera);
    for p in &o.players {
        println!("player: {p:?}");
    }
    for y in &o.ys {
        println!("y: {y:?}");
    }
    for g in &o.gates {
        println!("gate: {g:?}");
    }
}

pub fn observe(
    (console, file, profile): (&str, &str, &str),
    to_level: &str,
    play: &str,
    probes: &[usize],
) {
    let Some(mut rig) = Rig::open(console, file, Some(profile)) else {
        return;
    };
    rig.script(to_level);
    println!(
        "in level: scene {:?} after {} frames",
        rig.last_scene,
        rig.st.frame_count()
    );
    let ram = |rig: &Rig| -> Vec<u8> {
        probes
            .iter()
            .map(|&o| {
                let a = if rig.snes {
                    0x7E_0000 + o as u32
                } else {
                    o as u32
                };
                rig.st.peek_bus(a)
            })
            .collect()
    };
    println!("start: {:02x?}", ram(&rig));
    for tok in play.split_whitespace() {
        rig.script(tok);
        println!(
            "{tok}: scene {:?} {:02x?}",
            rig.last_scene.map(|s| s % 1000),
            ram(&rig)
        );
    }
    describe(&rig.finder.observed());
    for off in probes {
        println!("{}", rig.finder.probe(*off));
    }
    for off in probes {
        println!("{}", rig.finder.probe_slots(*off));
    }
}

// ---- acceptance: discovered table vs the hand-made one -------------------------

use retroforge::scout::{LiveView, ScoutDriver, ScoutPlan, ScoutSetup, ScoutTelemetry};
use retroforge::scout_discovery::{discover, plan_of, Context};
use rf_enhance::scene_identity::SceneId;
use std::sync::atomic::Ordering::Relaxed;

pub fn no_scrolls(_: &EmuStepper) -> Option<[Option<rf_enhance::canvas::LayerScroll>; 4]> {
    None
}

/// Scout the machine the rig is at with `plan` through the real driver (the
/// worker thread, the self-check, the ladder, the merge) and return the width
/// of the scene's first canvas layer, and the telemetry line.
pub fn scout_width(rig: &mut Rig, rom: &[u8], plan: ScoutPlan, secs: u64) -> (usize, String) {
    let scene = rig.live.current_scene().expect("a scene");
    let size = if rig.snes { (256, 224) } else { (256, 240) };
    let scrolls: retroforge::scout::ScrollReader = if rig.snes {
        retroforge::core_thread::snes_layer_scrolls
    } else {
        no_scrolls
    };
    let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
    let mut driver = ScoutDriver::default();
    driver.set(Some(ScoutSetup {
        plan,
        telemetry: telemetry.clone(),
    }));
    let t0 = std::time::Instant::now();
    // Bounded: `secs` seconds of 10 ms ticks.
    while t0.elapsed() < std::time::Duration::from_secs(secs) {
        driver.tick(LiveView {
            stepper: &rig.st,
            rom,
            canvases: &mut rig.live,
            scene,
            size,
            scrolls,
        });
        if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let width = rig
        .live
        .scene_canvases(scene.0)
        .map_or(0, |set| set.iter().map(|c| c.width()).max().unwrap_or(0));
    let line = format!(
        "started {} finished {} gave_up {} aborted {} frames {} cells {} agreement {:?}\u{2030}; passes ended {:?}; {}",
        telemetry.started.load(Relaxed),
        telemetry.finished.load(Relaxed),
        telemetry.gave_up.load(Relaxed),
        telemetry.aborted.load(Relaxed),
        telemetry.frames.load(Relaxed),
        telemetry.cells.load(Relaxed),
        telemetry.agreement_permille(),
        telemetry.last_ends.lock().map(|e| e.clone()).unwrap_or_default(),
        telemetry.last_summary().unwrap_or_default(),
    );
    // Ticket W33-08: which way each pass went.
    let dirs: Vec<String> = telemetry
        .last_report
        .lock()
        .ok()
        .and_then(|r| r.clone())
        .map(|r| r.passes.iter().map(|p| format!("{:?}", p.dir)).collect())
        .unwrap_or_default();
    let line = format!("passes {dirs:?}; {line}");
    (width, line)
}

/// One game: play the session, find the table, scout with it, and (when the
/// profile has a hand-made `[scout]`) scout with that too.
pub fn discover_game(
    (console, file, profile): (&str, &str, &str),
    to_level: &str,
    play: &str,
    hand: bool,
) -> Option<(rf_enhance::scout_finder::Skeleton, usize, Option<usize>)> {
    // `RF_HINTS=0`: the live model gets no profile scene hints.
    let hints = std::env::var("RF_HINTS").map_or(true, |v| v != "0");
    let mut rig = Rig::open(console, file, hints.then_some(profile))?;
    let path = rom_file(console, file)?;
    let rom = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    rig.script(to_level);
    rig.script("w60");
    let scene = SceneId(rig.last_scene.expect("a level scene"));
    let (snap_find, snap_found, snap_hand) = (
        rig.st.snapshot().expect("snapshot"),
        rig.st.snapshot().expect("snapshot"),
        rig.st.snapshot().expect("snapshot"),
    );
    let seed = rig.live.fork_for_scout();
    // The session.
    rig.script(play);
    let observed = rig.finder.observed();
    describe(&observed);
    let scrolls: retroforge::scout::ScrollReader = if rig.snes {
        retroforge::core_thread::snes_layer_scrolls
    } else {
        no_scrolls
    };
    let size = if rig.snes { (256, 224) } else { (256, 240) };
    let ctx = Context {
        rom: &rom,
        live: &seed,
        scene,
        size,
        snes: rig.snes,
        scrolls,
        scene_key: rig.scene_key.clone(),
    };
    let t0 = std::time::Instant::now();
    let (skeleton, report) = match discover(snap_find, ctx, &observed) {
        Ok(ok) => ok,
        Err(e) => {
            println!("DISCOVERY FAILED: {e}");
            if let retroforge::scout_discovery::DiscoveryError::NothingFollows(r) = &e {
                for t in &r.trials {
                    println!("  trial: {t}");
                }
            }
            return None;
        }
    };
    for t in &report.trials {
        println!("  trial: {t}");
    }
    println!(
        "discovery took {:.1}s\n{}",
        t0.elapsed().as_secs_f32(),
        skeleton.to_toml()
    );
    // The table must be a valid [scout] table of a profile.
    let doc = format!(
        "[meta]\nprofile_version = \"0.1\"\ntitle = \"T\"\nconsole = \"{console}\"\nregion = \"ntsc\"\nauthors = [\"t\"]\nsources = []\n[[identity]]\nsha256 = \"{}\"\nrevision = \"t\"\n[capabilities]\nfull_level = false\nhud_separation = false\nentity_overlay = false\nwidescreen = \"decoded\"\n\n{}",
        "0".repeat(64),
        skeleton.to_toml()
    );
    match rf_profiles::load_str(&doc) {
        Ok(_) => println!("the table loads as a profile [scout]"),
        Err(e) => panic!("the discovered table is not a valid profile table: {e}\n{doc}"),
    }
    let mut found = plan_of(&skeleton);
    found.settle_frames = 0;
    found.scene_key = rig.scene_key.clone();
    rig.st.restore(&snap_found).expect("restore");
    rig.reset();
    rig.script("w60");
    let (w_found, line) = scout_width(&mut rig, &rom, found, 600);
    println!("DISCOVERED table: width {w_found}; {line}");
    let w_hand = if hand {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(profile);
        let outcome = rf_profiles::load_file(&p).expect("profile");
        let mut plan = ScoutPlan::from_profile(outcome.profile.scout.as_ref().expect("[scout]"));
        plan.settle_frames = 0;
        plan.scene_key = rig.scene_key.clone();
        rig.st.restore(&snap_hand).expect("restore");
        rig.reset();
        rig.script("w60");
        let (w, line) = scout_width(&mut rig, &rom, plan, 600);
        println!("HAND-MADE table: width {w}; {line}");
        Some(w)
    } else {
        None
    };
    println!(
        "WIDTHS discovered {w_found} hand {w_hand:?}{}",
        w_hand.map_or(String::new(), |h| format!(
            " ({:.0}% of the hand-made table)",
            100.0 * w_found as f64 / h.max(1) as f64
        ))
    );
    Some((skeleton, w_found, w_hand))
}

pub fn accept(game: (&str, &str, &str), to_level: &str, play: &str) {
    let Some((_, found, Some(hand))) = discover_game(game, to_level, play, true) else {
        return;
    };
    assert!(
        found * 10 >= hand * 9,
        "the discovered table reached {found} of the hand-made {hand}: under 90%"
    );
}
