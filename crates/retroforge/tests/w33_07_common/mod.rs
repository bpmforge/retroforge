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
use rf_enhance::scout_finder::{Frame, Observed, Pad, ScoutFinder, Vert};
use rf_input::{NesButton, SnesButton};

pub struct Sink {
    pub bundle: FrameBundleBuilder,
    /// The picture as the player sees it (screenshots of an explored session).
    pub fb: rf_renderer::FrameBuffer,
}

impl Sink {
    pub fn new(w: u16, h: u16) -> Self {
        // As the app has it with widescreen on: the sub screen and the per-line registers are kept.
        let mut bundle = FrameBundleBuilder::new(w, h);
        bundle.set_capture_wide(true);
        Self {
            bundle,
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
        self.bundle.sub_scanline(y, pixels, fixed_color);
        self.fb.sub_scanline(y, pixels, fixed_color);
    }
    fn wants_line_registers(&self) -> bool {
        self.bundle.wants_line_registers()
    }
    fn line_registers(&mut self, y: u16, regs: &rf_core_api::LineRegisters) {
        self.bundle.line_registers(y, regs);
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
    /// The scroll writes of the last frame (NES), x then y low bytes.
    pub last_scroll: Vec<(u8, u8)>,
    /// The last frame's events (a probe reads the bands from them).
    pub last_events: Vec<CoreEvent>,
    /// W35-05: the hash of the last frame's indexed picture.
    pub shown: u64,
    /// W35-11: the last frame's picture was one flat colour.
    pub flat: bool,
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
        let mut finder = ScoutFinder::new(len, if snes { 2 } else { 1 });
        if !snes {
            finder.set_save_ram_from(0x800);
        }
        Some(Self {
            st,
            sink: Sink::new(w, h),
            live: CanvasModel::new(),
            finder,
            snes,
            scene_key,
            lag: ScrollLag::default(),
            last_scene: None,
            rom,
            find: None,
            find_out: Vec::new(),
            last_scroll: Vec::new(),
            last_events: Vec::new(),
            shown: 0,
            flat: false,
        })
    }

    /// Run one frame with the pad `mask` (NES bits or SNES bits), feeding the
    /// live model and the finder.
    pub fn frame(&mut self, mask: u16) {
        let mut input = InputFrame::empty();
        input.ports[0] = mask;
        self.st.latch_and_advance_frame(input, &mut self.sink);
        let bundle = self.sink.bundle.take(self.st.frame_count());
        self.shown = rf_enhance::scout_finder::picture_hash(&bundle.video);
        self.flat = retroforge::scout::picture_is_flat(&bundle.video);
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
            // The app scrolls the first layer the widescreen policy widens: the first one that is on.
            let main = views.iter().find(|v| v.enabled).unwrap_or(&views[0]);
            let read = FrameScrolls {
                scene: Some((main.hofs, main.vofs)),
                layers: retroforge::core_thread::snes_layer_scrolls(&self.st),
            };
            let drawn = self.lag.drawn_with(self.st.frame_count(), read);
            let feed = FrameFeed::registers(scrolls_for(&bundle, &drawn, &read))
                .with_scene_scroll(drawn.scene)
                .with_scene_hints(hints);
            scene = self.live.observe_frame(&bundle, &feed);
            scroll = retroforge::scout_find::finder_snes_scroll(&self.st).expect("snes");
        } else {
            scene = self
                .live
                .observe_frame(&bundle, &FrameFeed::bands().with_scene_hints(hints));
            let (mut xs, mut ys) = (Vec::new(), Vec::new());
            self.last_scroll.clear();
            self.last_events = bundle.events.clone();
            for e in &bundle.events {
                if let CoreEvent::ScrollWrite { x, y, .. } = e {
                    xs.push((x & 0xFF) as u8);
                    ys.push((y & 0xFF) as u8);
                    self.last_scroll.push(((x & 0xFF) as u8, (y & 0xFF) as u8));
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
        let (left, right, up, down, face_bits) = if self.snes {
            (
                1u16 << SnesButton::Left.bit(),
                1 << SnesButton::Right.bit(),
                1 << SnesButton::Up.bit(),
                1 << SnesButton::Down.bit(),
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
                1 << NesButton::Up.bit(),
                1 << NesButton::Down.bit(),
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
        let vert = if mask & down != 0 {
            Vert::Down
        } else if mask & up != 0 {
            Vert::Up
        } else {
            Vert::Idle
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
            vert,
            jump: face,
            scroll: &layers,
            scene,
            start: mask
                & (1 << if self.snes {
                    SnesButton::Start.bit()
                } else {
                    NesButton::Start.bit()
                })
                != 0,
            shown: self.shown,
        });
    }

    /// Start the live model, the scroll lag and the finder afresh (a machine
    /// restored from a snapshot is a new session).
    pub fn reset(&mut self) {
        let len = if self.snes { 0x2000 } else { 0x800 + 0x2000 };
        self.live = CanvasModel::new();
        self.finder = ScoutFinder::new(len, if self.snes { 2 } else { 1 });
        if !self.snes {
            self.finder.set_save_ram_from(0x800);
        }
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

/// Boot, menus, the map at Yoshi's House, (W35-05: into the house again, which Mario can walk in,
/// and out of its right end: a second room played in), left to Yoshi's Island 1, enter, and
/// wait for the fade-in (`w32_07_scout_smw::reach_yi1` as tokens).
pub const SMW_TO_LEVEL: &str = "w200 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 w300 A8 w408 A8 w200 R40 w20 L40 w20 R40 w20 L40 w20 R40 w20 L40 w20 R60 w20 R200 w300 L8 w158 A8 w158";
pub const MMX_TO_LEVEL: &str = "w300 S6 w114 S6 w114 S6 w114 S6 w114 S6 w400";
pub const SMB3_TO_LEVEL: &str = "w300 S6 w120 S6 w400 R8 w60 U8 w60 A8 w300";
pub const MM2_TO_LEVEL: &str = "w400 S6 w120 S6 w300 S6 w300 U8 w20 S6 w800";

/// What the player does once in the level: stand, jump four times (standing),
/// walk into the left wall, then right a long way (jumping now and again).
pub const SMW_PLAY: &str = "L2 w3 B8 w52 B8 w52 B8 w52 B8 w52 R+B14 R22 R+B14 R22 S6 w100 S6 w60 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22";
pub const SMB3_PLAY: &str = "L30 w20 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 w20 S6 w80 S6 w40 A8 w60 A8 w60 A8 w60 A8 w60 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24 R+B14 R+B+A24";
/// Mega Man X: the same, with B to jump.
pub const MMX_PLAY: &str = "L30 w5 R40 R+B20 R40 R+B20 R40 R+B20 R40 R+B20 w20 S6 w80 S6 w40 B8 w60 B8 w60 B8 w60 B8 w60 R40 R+B20 R40 R+B20 R40 R+B20 R40 R+B20";
pub const MM2_PLAY: &str = "L30 w5 A8 w60 A8 w60 A8 w60 A8 w60 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 S6 w80 S6 w40 w400 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15 R25 R+A15";

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
    // W35-03: the gate ranking with its contrast evidence (more than the finder keeps).
    for (i, g) in rig.finder.gates_ranked(20000).iter().enumerate() {
        println!(
            "RANK {i:3} off {:#06x} = {:#04x} constancy {:.3} differs {:.2} sync {:.2} cover {:.2} steps {:.2} off {} cuts {} mode {:.3} apart {:.2}/{}",
            g.offset, g.value, g.constancy, g.differs, g.cut_sync, g.cut_cover, g.cut_steps, g.off_cut, g.cuts, g.mode_score, g.freeze_apart, g.frozen_frames
        );
    }
    // W35-05: the values the top-ranked bytes (and the probes) take in each state.
    let top: Vec<usize> = rig
        .finder
        .gates_ranked(4)
        .iter()
        .map(|g| g.offset)
        .chain(probes.iter().copied())
        .collect();
    for off in top {
        println!("{}", rig.finder.state_probe(off));
    }
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
            blank: false,
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
    game: (&str, &str, &str),
    to_level: &str,
    play: &str,
    hand: bool,
) -> Option<(rf_enhance::scout_finder::Skeleton, usize, Option<usize>)> {
    discover_game_with(game, to_level, play, hand, None)
}

/// [`discover_game`], with a second area (W35-11): after the session's play the machine is put
/// back to the `snapshot` (made by another run of the game, in another level) and the second
/// script is played there, so the finder has seen two areas.
pub fn discover_game_with(
    (console, file, profile): (&str, &str, &str),
    to_level: &str,
    play: &str,
    hand: bool,
    second: Option<(&retroforge::stepper::MachineSnapshot, &str)>,
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
    if let Some((snap, play2)) = second {
        rig.st.restore(snap).expect("restore the second area");
        rig.script(play2);
    }
    for o in std::env::var("RF_PROBE")
        .unwrap_or_default()
        .split(',')
        .filter_map(|o| usize::from_str_radix(o.trim(), 16).ok())
    {
        println!("{}", rig.finder.state_probe(o));
    }
    for c in rig.finder.clean_gates(12) {
        println!(
            "clean gate {:#06x} = {:#04x} +{:02x?} off-cut {}",
            c.offset,
            c.value,
            &c.also[..usize::from(c.also_n)],
            c.off_cut
        );
    }
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
        vertical: None,
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

// ---- Super Metroid (W35-02) ------------------------------------------------------------------

pub const SM: (&str, &str, &str) = (
    "snes",
    "Super Metroid (Japan, USA) (En,Ja).zip",
    "../../profiles/snes/super-metroid/profile.toml",
);

/// Boot, option screen, new game, the story pages (A advances each), to Ceres's first room with
/// Samus on the elevator platform in control (about frame 11,800).
pub fn sm_to_free_play() -> String {
    let mut s = String::from("w300 w300 S6 w120 S6 w120 A6 w120 A6 w120 S6 w120 A6 w600");
    for _ in 0..55 {
        s += " A6 w150";
    }
    s + " w500 w600 w300"
}

/// Free play down the shaft (the screen scrolls up and down) and through its door into the
/// corridor, a room two screens wide, then the door's fade.
pub const SM_SHAFT: &str = "R40 w60 R40 w60 L40 w60 R40 w60 D10 w60 L40 w60 L40 w60 R40 w60 D10 w60 R40 R+A25 R30 R+A25 R30 w250";

/// In the corridor (its entry door is on the left: never walk left from the entry): right over
/// the step, left a little, six standing jumps, right and left again, right to the far end, stand.
pub const SM_PLAY: &str = "R+A30 R60 L45 w20 A8 w50 A8 w50 A8 w50 A8 w50 A8 w50 A8 w50 S6 w100 S6 w80 R+A30 R60 L50 R50 L50 R+A30 R60 R+A30 w60";

/// The finder as the core thread runs it ("Find scout settings for this game"): one session from
/// free play (the shaft, the door, the corridor), then standing still until the trials answer.
/// `after_shaft` runs between the shaft and the corridor play (a snapshot there starts a scout at
/// the corridor's door).
pub fn sm_find_table(
    rig: &mut Rig,
    secs: u64,
    after_shaft: &mut dyn FnMut(&mut Rig),
) -> Box<rf_enhance::scout_finder::Skeleton> {
    use retroforge::scout_find::{Out, ScoutFind};
    rig.find = Some(ScoutFind::new(true));
    rig.script(SM_SHAFT);
    after_shaft(rig);
    rig.script(SM_PLAY);
    let t0 = std::time::Instant::now();
    let mut found = None;
    // Bounded: `secs` seconds of idle frames.
    while t0.elapsed() < std::time::Duration::from_secs(secs) && found.is_none() {
        rig.script("w30");
        for out in rig.find_out.drain(..) {
            if let Out::Done(r) = out {
                found = Some(r);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    found.expect("an answer").expect("a table, not an error")
}

// ---- Castlevania (W35-03) ----------------------------------------------------------------------

pub const CV: (&str, &str, &str) = (
    "nes",
    "Castlevania (USA).zip",
    "../../profiles/nes/castlevania/profile.toml",
);

/// Boot, title (Start), the intro walk to the gate: Simon stands in the courtyard, stage 1.
pub const CV_TO_STAGE: &str = "w400 S6 w600";
/// Left to the wall, six standing jumps (A), then right toward the door with a jump every 60
/// frames (Simon cannot turn in the air): the first half, up to the middle of the courtyard.
pub const CV_PLAY_HEAD: &str =
    "L50 w20 A8 w50 A8 w50 A8 w50 A8 w50 A8 w50 A8 w50 S6 w90 S6 w60 R60 R+A30 R60 R+A30 R60 R+A30 R60 R+A30";
/// The second half: on to the door, through it into the entrance hall, a few steps in, stand.
pub const CV_PLAY_TAIL: &str =
    "R60 R+A30 R60 R+A30 R60 R+A30 R60 R+A30 R60 R+A30 R60 R60 R60 R60 w60";
pub const CV_PLAY: &str = concat!(
    "L50 w20 A8 w50 A8 w50 A8 w50 A8 w50 A8 w50 A8 w50 S6 w90 S6 w60 R60 R+A30 R60 R+A30 R60 R+A30 R60 R+A30",
    " ",
    "R60 R+A30 R60 R+A30 R60 R+A30 R60 R+A30 R60 R+A30 R60 R60 R60 R60 w60"
);

/// The finder as the core thread runs it ("Find scout settings for this game"), from the
/// courtyard: the session, then standing still until the trials answer. `mid_block` runs between
/// the two halves of the walk (a snapshot there starts a scout in the middle of block 1).
pub fn cv_find_table(
    rig: &mut Rig,
    secs: u64,
    mid_block: &mut dyn FnMut(&mut Rig),
) -> Box<rf_enhance::scout_finder::Skeleton> {
    use retroforge::scout_find::{Out, ScoutFind};
    rig.find = Some(ScoutFind::new(false));
    rig.script(CV_PLAY_HEAD);
    mid_block(rig);
    rig.script(CV_PLAY_TAIL);
    let t0 = std::time::Instant::now();
    let mut found = None;
    // Bounded: `secs` seconds of idle frames.
    while t0.elapsed() < std::time::Duration::from_secs(secs) && found.is_none() {
        rig.script("w30");
        for out in rig.find_out.drain(..) {
            if let Out::Done(r) = out {
                found = Some(r);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    found.expect("an answer").expect("a table, not an error")
}

// ---- A Link to the Past (W35-04) ---------------------------------------------------------------

pub const ALTTP: (&str, &str, &str) = (
    "snes",
    "Legend of Zelda, The - A Link to the Past (USA).zip",
    "../../profiles/snes/the-legend-of-zelda-a-link-to-the-past/profile.toml",
);

/// Boot, file select, a name, the first telepathy speech (A advances each page, slowly), up from
/// bed into Link's house, to the door, outside in the rain: free play on the first overworld area.
pub fn alttp_to_free_play() -> String {
    let mut s = String::from("w600 w300 S6 w300 S6 w300 S6 w300 A6 w60 S6 w300 A6 w300 w600");
    for _ in 0..16 {
        s += " A3 w150";
    }
    s + " w300 R32 w10 D60 w10 D60 w10 D40 L14 w5 D40 w100 D40 w100 D40 w100 w300"
}

/// One game-world session on the first overworld area, from the spot outside Link's house: back
/// in through the door and out again (two scenes), down the ledge onto the road, west along it to
/// the trees (a long vertical corridor there), north and south in it, east again nearly to the
/// soldiers (never into them), west and east once more across the page boundaries, with a press
/// of the action buttons now and again (they move nothing). Pure directions: no diagonals.
/// Zelda's telepathy ("Help me...", mode `$10 = 0x0e`) breaks into free play some 1,500 frames after
/// the house, wherever Link is, and holds him until A has paged through it; so does a soldier's
/// scolding. Five presses, a long wait after each (the text types itself out first), cost nothing
/// when there is no box.
pub fn alttp_dismiss() -> String {
    " A3 w150".repeat(5)
}

/// The first half of the session: back into the house and out again, down onto the road and a
/// little west: Link stands in the middle of the road.
pub fn alttp_play_head() -> String {
    format!("{} {}", alttp_play_walk(), alttp_play_start())
}

/// W35-06: the head's walk alone (the area edge's scroll has finished: a Start pressed during it is
/// swallowed and the next one opens the menu, which a snapshot "on the road" must not catch).
pub fn alttp_play_walk() -> String {
    "w60 U60 w200 D60 w200 D80 w30 L120 w90".to_string()
}

/// W35-06: the head's Start pause (W35-05) and the dismissals after it.
pub fn alttp_play_start() -> String {
    format!("S6 w100 S6 w100{}", alttp_dismiss())
}

/// The rest of the session: on west to the trees, the vertical corridor there, east and west
/// across the page boundaries (a dismissal after the long legs), and back east onto the road.
pub fn alttp_play_tail() -> String {
    let d = alttp_dismiss();
    format!(
        "L210 w30 U200 w30 D200 w30 U100 w20 D100 w20 A6 w30 B6 w30 {d} \
         R330 w30{d} L330 w30{d} R330 w30{d} U200 w30 D200 w30 L330 w30{d} R200 w30 U120 w30 D120 w30 R100 w60"
    )
}

/// The whole session.
pub fn alttp_play() -> String {
    format!("{} {}", alttp_play_head(), alttp_play_tail())
}

/// The same session from the road (no trip through the house door): for the headless discovery,
/// which snapshots where the session starts and needs a player who can walk from there.
pub fn alttp_play_road() -> String {
    alttp_play_tail()
}

/// The finder as the core thread runs it ("Find scout settings for this game"), from the spot
/// outside Link's house: the session, then standing still until the trials answer. `mid_road`
/// runs between the two halves (a snapshot there starts a scout in the middle of the road).
pub fn alttp_find_table(
    rig: &mut Rig,
    secs: u64,
    mid_road: &mut dyn FnMut(&mut Rig),
) -> Box<rf_enhance::scout_finder::Skeleton> {
    use retroforge::scout_find::{Out, ScoutFind};
    rig.find = Some(ScoutFind::new(true));
    rig.script(&alttp_play_walk());
    mid_road(rig);
    rig.script(&alttp_play_start());
    rig.script(&alttp_play_tail());
    let t0 = std::time::Instant::now();
    let mut found = None;
    // Bounded: `secs` seconds of idle frames.
    while t0.elapsed() < std::time::Duration::from_secs(secs) && found.is_none() {
        rig.script("w30");
        for out in rig.find_out.drain(..) {
            if let Out::Done(r) = out {
                found.get_or_insert(r);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    found.expect("an answer").expect("a table, not an error")
}

// ---- The Legend of Zelda, NES (W35-04) ---------------------------------------------------------

pub const ZELDA1: (&str, &str, &str) = (
    "nes",
    "Legend of Zelda, The (USA).zip",
    "../../profiles/nes/legend-of-zelda/profile.toml",
);

/// Boot, title, "REGISTER YOUR NAME", an `A`, back to the select screen with the name in slot 1,
/// start it: Link stands on the first overworld screen.
pub const Z1_TO_PLAY: &str =
    "w300 S6 w300 S6 w300 A6 w30 E6 w20 E6 w20 E6 w20 S6 w200 E6 w20 E6 w20 E6 w20 S6 w400";

/// W35-07: the session a player gives the finder on the first overworld screen: walk over the
/// screen's right edge and back, over and over (the screens flip).
pub const Z1_WALK: &str = "R60 R60 R60 w100 L60 L60 L60 L60 w100 R60 R60 R60 w100 L60 L60 L60 L60 w100 R60 R60 R60 w100 L60 L60 L60 L60 w100 R60 R60 R60 w100 L60 L60 L60 L60 w100";

/// The finder as the core thread runs it ("Find scout settings for this game"), from the first
/// overworld screen: the session, then standing still until the trials answer.
pub fn z1_find_table(rig: &mut Rig, secs: u64) -> Box<rf_enhance::scout_finder::Skeleton> {
    use retroforge::scout_find::{Out, ScoutFind};
    rig.find = Some(ScoutFind::new(false));
    rig.script(Z1_WALK);
    let t0 = std::time::Instant::now();
    let mut found = None;
    // Bounded: `secs` seconds of idle frames.
    while t0.elapsed() < std::time::Duration::from_secs(secs) && found.is_none() {
        rig.script("w30");
        for out in rig.find_out.drain(..) {
            if let Out::Done(r) = out {
                found.get_or_insert(r);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    found.expect("an answer").expect("a table, not an error")
}
