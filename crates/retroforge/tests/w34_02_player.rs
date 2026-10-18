//! Ticket W34-02 on real machines: the player cluster is found, and the
//! camera's orientation called, from the sprites, the pad and the scroll
//! alone (no profile, no RAM).
//!
//! RF-Scroller and RF-Scroller-S are in-repo fixtures and run in the normal
//! gate. Super Mario World, Super Mario Bros. 3 and A Link to the Past need
//! the user's ROMs and are `#[ignore]`d (also timing):
//!
//! ```text
//! cargo test --release -p retroforge --test w34_02_player -- --ignored --nocapture
//! RF_SCREENS_ROMS_DIR=/path/to/Roms overrides the ROM folder.
//! ```
//!
//! Frames-to-find count from the first frame of the scripted play (the game
//! already past its title/menus and under the player's control).

use std::path::{Path, PathBuf};
use std::time::Instant;

use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreEvent, CoreSink, EventMask, FrameBundleBuilder, InputFrame, PpuPixel};
use rf_enhance::heights::{
    Dirs, FrameInput, Orientation, OrientationGuess, PlayerSource, PlayerTracker, SpriteBox,
};
use rf_enhance::scene_tracker::SceneTracker;
use rf_input::{NesButton, SnesButton};

const FIND_BUDGET: u32 = 120;

/// The bundle builder plus a frame buffer, so a run can be looked at.
struct Sink {
    bundle: FrameBundleBuilder,
    fb: rf_renderer::FrameBuffer,
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

struct Game {
    st: EmuStepper,
    sink: Sink,
    scene: SceneTracker,
    tracker: PlayerTracker,
    sprites: Vec<SpriteBox>,
    snes: bool,
    /// SNES camera: BG1 scroll registers, unwrapped (10 bits).
    snes_cam: (i64, i64, Option<(u16, u16)>),
    size: (u16, u16),
    /// Time spent in sprite decode + cluster + detect, per frame (us).
    cost: Vec<f64>,
}

impl Game {
    fn open(raw: &[u8]) -> Self {
        let mut st = EmuStepper::open(raw).expect("opens");
        st.set_event_mask(EventMask::ALL);
        let snes = st.bg_layer_views().is_some();
        let size = if snes { (256, 224) } else { (256, 240) };
        Game {
            st,
            sink: Sink {
                bundle: FrameBundleBuilder::new(size.0, size.1),
                fb: rf_renderer::FrameBuffer::with_size(usize::from(size.0), usize::from(size.1)),
            },
            scene: SceneTracker::new(),
            tracker: PlayerTracker::new(),
            sprites: Vec::new(),
            snes,
            snes_cam: (0, 0, None),
            size,
            cost: Vec::new(),
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

    /// One frame; `watch` feeds the detector as well.
    fn frame(&mut self, mask: u16, watch: bool) {
        let mut f = InputFrame::empty();
        f.ports[0] = mask;
        self.st.latch_and_advance_frame(f, &mut self.sink);
        let bundle = self.sink.bundle.take(self.st.frame_count());
        self.scene.observe_frame(
            &bundle.video,
            bundle.width,
            bundle.height,
            &bundle.events,
            &[],
        );
        if watch {
            let camera = self.camera();
            let t = Instant::now();
            self.st.sprite_boxes(
                (i32::from(self.size.0), i32::from(self.size.1)),
                &mut self.sprites,
            );
            self.tracker.observe(&FrameInput {
                sprites: &self.sprites,
                pad: self.dirs(mask),
                camera,
                hint_x: None,
                hint_y: None,
            });
            self.cost.push(t.elapsed().as_secs_f64() * 1e6);
        }
    }

    /// The camera in world pixels. SNES: BG1's registers, unwrapped. NES: the
    /// scene tracker (picture + scroll writes).
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

    fn run(&mut self, mask: u16, n: u32, watch: bool) {
        for _ in 0..n {
            self.frame(mask, watch);
        }
    }

    /// Save the picture (RF_SHOTS=dir) to see where a boot script is.
    fn shot(&self, name: &str) {
        if let Some(dir) = std::env::var_os("RF_SHOTS") {
            let path = Path::new(&dir).join(format!("{name}.png"));
            image::save_buffer(
                &path,
                self.sink.fb.rgba(),
                self.sink.fb.width() as u32,
                self.sink.fb.height() as u32,
                image::ColorType::Rgba8,
            )
            .expect("png");
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

    /// Start the detector afresh: the scripted play begins now.
    fn begin(&mut self) {
        self.tracker = PlayerTracker::new();
        self.cost.clear();
    }

    fn report(&self, label: &str) -> (Option<u32>, OrientationGuess) {
        let o = self.tracker.orientation();
        let found = self.tracker.first_found().map(|p| p.found_at);
        let travel = self.tracker.scroll_travel();
        println!(
            "PLAYER {label}: found at frame {found:?} ({:?}), entity {:?}; orientation {:?} \
             confidence {} via {:?}; scroll travel {travel:?}; votes [gravity, anti, steady, inert] {:?}",
            self.tracker.first_found().map(|p| p.source),
            self.tracker.first_found().map(|p| p.entity),
            o.orientation,
            o.confidence,
            o.source,
            self.tracker.vertical_votes(),
        );
        if std::env::var_os("RF_TRACE").is_some() {
            for t in self.tracker.track_evidence() {
                println!("TRACK {t:?}");
            }
        }
        let mut c = self.cost.clone();
        if !c.is_empty() {
            c.sort_by(f64::total_cmp);
            println!(
                "COST {label}: sprite decode + cluster + detect median {:.1} us, p95 {:.1} us, \
                 max {:.1} us over {} frames",
                c[c.len() / 2],
                c[(c.len() - 1) * 95 / 100],
                c[c.len() - 1],
                c.len()
            );
        }
        (found, o)
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

fn check(label: &str, g: &Game, want: Orientation) -> (Option<u32>, OrientationGuess) {
    let (found, o) = g.report(label);
    let at = found.unwrap_or_else(|| panic!("{label}: no player found"));
    assert!(at <= FIND_BUDGET, "{label}: found at {at} > {FIND_BUDGET}");
    assert_eq!(
        g.tracker.first_found().map(|p| p.source),
        Some(PlayerSource::Pad)
    );
    assert_eq!(o.orientation, want, "{label}: {o:?}");
    (found, o)
}

#[test]
fn rf_scroller_nes() {
    let Some(raw) = fixture("nes/rf-scroller/build/rf-scroller.nes") else {
        return;
    };
    let mut g = Game::open(&raw);
    g.run(0, 120, false);
    g.tap(g.mask_of("S"), 4);
    g.run(0, 60, false);
    g.begin();
    g.script("w4 R90 w30 U20 D20 w10 L20 w20 R230 w20", true);
    check("rf-scroller", &g, Orientation::SideView);
}

#[test]
fn rf_scroller_s_snes() {
    let Some(raw) = fixture("snes/rf-scroller-s/build/rf-scroller-s.sfc") else {
        return;
    };
    let mut g = Game::open(&raw);
    g.run(0, 120, false);
    g.tap(g.mask_of("S"), 4);
    g.run(0, 60, false);
    g.begin();
    // The fixture draws its "player" as nothing (OBJ is off, 128 sprites sit
    // parked in a corner): `player_x` is a variable and
    // the camera follows it. There is no sprite to find; the scroll axis
    // still gives the orientation.
    g.script("w4 R230 w20", true);
    let o = g.tracker.orientation();
    println!(
        "PLAYER rf-scroller-s: no player sprite ({:?}); orientation {o:?}; travel {:?}",
        g.tracker.player(),
        g.tracker.scroll_travel()
    );
    assert!(
        g.tracker.player().is_none(),
        "no sprite answers the pad: nothing to find"
    );
    assert_eq!(o.orientation, Orientation::SideView, "{o:?}");
}

const PLAY: &str = "w4 R35 w20 L25 w20 {jump}24 w50 U15 w5 D15 w30";

fn platformer(label: &str, g: &mut Game, jump: &str) {
    g.shot(&format!("{label}-start"));
    g.begin();
    g.script(&PLAY.replace("{jump}", jump), true);
    g.shot(&format!("{label}-end"));
    check(label, g, Orientation::SideView);
}

/// Super Mario World to Yoshi's Island 1 (`w34_01_real_games::smw_yi1`).
#[test]
#[ignore = "real ROM; run deliberately in release"]
fn smw() {
    let Some(raw) = rom("snes", "Super Mario World (USA).zip") else {
        return;
    };
    let mut g = Game::open(&raw);
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
    platformer("smw", &mut g, "R+B");
}

/// Super Mario Bros. 3 to World 1-1 (`w30_screens::smb3_enter_level`).
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
    platformer("smb3", &mut g, "R+A");
}

/// A Link to the Past: boot, register a name (Start ends the name entry),
/// click through the opening text, then Link is in control in his house. The
/// script was found by looking at the picture (RF_SHOTS=dir keeps it).
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
    // The opening text needs A to move on until Link is up and in control.
    for _ in 0..24 {
        g.tap(g.mask_of("A"), 4);
        g.run(0, 50, false);
    }
    g.shot("alttp-start");
    g.begin();
    g.script("w4 D30 w15 R40 w15 U20 w15 L40 w15 D20 w20", true);
    g.shot("alttp-end");
    check("alttp", &g, Orientation::TopDown);
}
