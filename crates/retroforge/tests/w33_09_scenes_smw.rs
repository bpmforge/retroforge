//! **W33-09: scene identity on Super Mario World (USA).** ROM-gated
//! (`--ignored`), headless.
//!
//! * `smw_scenes_pipe_room_and_back`: the acceptance run: map, Yoshi's Island
//!   1, the coin room behind its pipe, and back out, with the profile's scene
//!   key and the per-layer canvases fed as the core thread feeds them.
//! * `smw_level_bytes`, `smw_pipe_key`, `smw_level_long`: how the
//!   `[[scene.level_id]]` rows were found (RAM diffs between map, level and
//!   room). `smw_find_pipe` shows the level has no pipe a player can reach by
//!   walking; `smw_pipe_try` is the tool that entered it (`RF_POKE_X`,
//!   `RF_POKE=71=06`, `RF_TAIL`).
//!
//! Evidence: docs/research/W33-09-scenes.md.

use std::path::{Path, PathBuf};

use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreSink, FrameBundleBuilder, InputFrame, PpuPixel};
use rf_input::SnesButton;

#[path = "../../rf-harness/tests/smw_drive/mod.rs"]
mod smw_drive;

const W: u16 = 256;
const H: u16 = 224;

fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root)
        .join("snes")
        .join("Super Mario World (USA).zip");
    p.exists().then_some(p)
}

/// Collects the picture as a `FrameBundleBuilder` does; every other channel
/// is dropped.
pub struct Sink {
    pub bundle: FrameBundleBuilder,
}

impl Default for Sink {
    fn default() -> Self {
        Self::new()
    }
}

impl Sink {
    pub fn new() -> Self {
        Self {
            bundle: FrameBundleBuilder::new(W, H),
        }
    }
}

impl CoreSink for Sink {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.bundle.video_scanline(y, pixels);
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, ev: rf_core_api::CoreEvent) {
        self.bundle.event(ev);
    }
}

fn pad(buttons: &[SnesButton]) -> InputFrame {
    let mut f = InputFrame::empty();
    f.ports[0] = buttons.iter().fold(0u16, |a, b| a | (1 << b.bit()));
    f
}

pub fn run(st: &mut EmuStepper, sink: &mut Sink, buttons: &[SnesButton], n: u32) {
    let input = pad(buttons);
    for _ in 0..n {
        st.latch_and_advance_frame(input, sink);
        drop(sink.bundle.take(st.frame_count()));
    }
}

fn tap(st: &mut EmuStepper, sink: &mut Sink, b: SnesButton, hold: u32) {
    run(st, sink, &[b], hold);
    run(st, sink, &[], 8);
}

/// Boot, menus, the map at Yoshi's House, left to Yoshi's Island 1, enter,
/// and wait for the fade-in: the same script `w30_screens::shot_widescreen`
/// runs through the GUI (Enter = Start, X = the A button).
pub fn reach_yi1(st: &mut EmuStepper, sink: &mut Sink) {
    // W37-00c: closed-loop, waits on the game mode (`smw_drive::to_yi1_direct`).
    smw_drive::to_yi1_direct(&mut |mask| {
        let mut input = InputFrame::empty();
        input.ports[0] = mask;
        st.latch_and_advance_frame(input, sink);
        drop(sink.bundle.take(st.frame_count()));
        st.peek_bus(smw_drive::MODE)
    });
}

fn open() -> Option<(EmuStepper, Sink)> {
    let path = rom_path().or_else(|| {
        println!("SKIP: Super Mario World (USA) not found");
        None
    })?;
    let raw = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    Some((EmuStepper::open(&raw).expect("opens"), Sink::new()))
}

fn bg1_hofs(st: &EmuStepper) -> u16 {
    st.bg_layer_views().expect("snes")[0].hofs
}

fn bg1_vofs(st: &EmuStepper) -> u16 {
    st.bg_layer_views().expect("snes")[0].vofs
}

fn btn(names: &str) -> Vec<SnesButton> {
    names
        .split('+')
        .filter_map(|n| match n {
            "A" => Some(SnesButton::A),
            "B" => Some(SnesButton::B),
            "X" => Some(SnesButton::X),
            "S" => Some(SnesButton::Start),
            "U" => Some(SnesButton::Up),
            "D" => Some(SnesButton::Down),
            "L" => Some(SnesButton::Left),
            "R" => Some(SnesButton::Right),
            _ => None,
        })
        .collect()
}

/// Script tokens: `wN` idle, `<buttons>N` hold N frames.
fn wram(st: &EmuStepper) -> Vec<u8> {
    (0..0x10000u32)
        .map(|a| st.peek_bus(0x7E_0000 + a))
        .collect()
}

fn constant_window(
    st: &mut EmuStepper,
    sink: &mut Sink,
    b: &[SnesButton],
    n: u32,
) -> Vec<Option<u8>> {
    let mut c: Vec<Option<u8>> = wram(st).into_iter().map(Some).collect();
    for _ in 0..n {
        run(st, sink, b, 1);
        for (slot, v) in c.iter_mut().zip(wram(st)) {
            if *slot != Some(v) {
                *slot = None;
            }
        }
    }
    c
}

#[test]
#[ignore = "needs the SMW ROM"]
fn smw_level_bytes() {
    let Some((mut st, mut sink)) = open() else {
        return;
    };
    // The map at Yoshi's House (before the left press), then YI1.
    run(&mut st, &mut sink, &[], 200);
    for i in 0..10 {
        tap(
            &mut st,
            &mut sink,
            if i % 2 == 0 {
                SnesButton::Start
            } else {
                SnesButton::A
            },
            6,
        );
        run(&mut st, &mut sink, &[], 100);
    }
    run(&mut st, &mut sink, &[], 300);
    tap(&mut st, &mut sink, SnesButton::A, 8);
    run(&mut st, &mut sink, &[], 400);
    let map_a = constant_window(&mut st, &mut sink, &[], 20);
    tap(&mut st, &mut sink, SnesButton::Left, 8);
    run(&mut st, &mut sink, &[], 150);
    let map_b = constant_window(&mut st, &mut sink, &[], 20);
    tap(&mut st, &mut sink, SnesButton::A, 8);
    run(&mut st, &mut sink, &[], 150);
    let lvl = constant_window(&mut st, &mut sink, &[], 20);
    println!("map(house) / map(YI1 node) / level, all distinct:");
    let n = lvl.len();
    let mut shown = 0;
    for a in 0..n {
        if let (Some(x), Some(y), Some(z)) = (map_a[a], map_b[a], lvl[a]) {
            if x != z && y != z && shown < 80 {
                println!("  7E{a:04X}: mapA={x:02X} mapB={y:02X} lvl={z:02X}");
                shown += 1;
            }
        }
    }
    println!(
        "{shown} bytes; hofs {} vofs {}",
        bg1_hofs(&st),
        bg1_vofs(&st)
    );
}

fn histogram(b: &rf_core_api::FrameBundle) -> [u32; 256] {
    let mut h = [0u32; 256];
    for p in &b.video {
        h[usize::from(p.palette_index)] += 1;
    }
    h
}

fn hist_dist(a: &[u32; 256], b: &[u32; 256]) -> f64 {
    let t: u32 = a.iter().sum();
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(x.abs_diff(*y)))
        .sum::<f64>()
        / f64::from(t.max(1))
        / 2.0
}

/// One frame with the pad held; returns the bundle.
fn frame(st: &mut EmuStepper, sink: &mut Sink, b: &[SnesButton]) -> rf_core_api::FrameBundle {
    st.latch_and_advance_frame(pad(b), sink);
    sink.bundle.take(st.frame_count())
}

fn to_yi1(st: &mut EmuStepper, sink: &mut Sink) {
    reach_yi1(st, sink);
}

/// Walk right in YI1 (star power poked), pressing Down every 20th frame;
/// print frames where the picture's colour histogram jumps and where the
/// camera x jumps.
#[test]
#[ignore = "needs the SMW ROM"]
fn smw_find_pipe() {
    let Some((mut st, mut sink)) = open() else {
        return;
    };
    to_yi1(&mut st, &mut sink);
    let mut prev: Option<[u32; 256]> = None;
    let mut last_cam = bg1_hofs(&st);
    let n: u32 = std::env::var("RF_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    for f in 0..n {
        st.poke_ram(0x7E_1490, 0xFF);
        let mut pad_now = btn("R+B");
        if f % 40 < 14 {
            pad_now.push(SnesButton::A);
        }
        if f % 60 >= 50 {
            pad_now = btn("D");
        }
        let b = frame(&mut st, &mut sink, &pad_now);
        let h = histogram(&b);
        if let Some(p) = &prev {
            let d = hist_dist(p, &h);
            if d > 0.25 {
                println!(
                    "f{f} hist jump {d:.2} lvlbyte 010B={:02X} 13BF={:02X}",
                    st.peek_bus(0x7E_010B),
                    st.peek_bus(0x7E_13BF)
                );
            }
        }
        prev = Some(h);
        let cam = bg1_hofs(&st);
        if cam.abs_diff(last_cam) > 100 {
            println!("f{f} camera {last_cam} -> {cam}");
        }
        last_cam = cam;
        if f % 300 == 0 {
            println!(
                "f{f} cam {cam} x={:#06x}",
                u16::from(st.peek_bus(0x7E_0094)) | u16::from(st.peek_bus(0x7E_0095)) << 8
            );
        }
    }
}

fn mark(st: &EmuStepper, tag: &str) {
    println!(
        "{tag}: mode100={:02X} 13BF={:02X} 0057={:02X}",
        st.peek_bus(0x7E_0100),
        st.peek_bus(0x7E_13BF),
        st.peek_bus(0x7E_0057),
    );
}

/// Save the bundle as a PNG through the CGRAM the frame was drawn with.
fn save_snes(st: &EmuStepper, b: &rf_core_api::FrameBundle, name: &str) {
    let cg = st.snes_debug_snapshot().expect("snes").cgram;
    let mut rgba = Vec::with_capacity(b.video.len() * 4);
    for p in &b.video {
        let i = usize::from(p.palette_index) * 2;
        let w = u16::from(cg[i]) | u16::from(cg[i + 1]) << 8;
        let c = |s: u16| (((w >> s) & 31) as u8) << 3;
        rgba.extend_from_slice(&[c(0), c(5), c(10), 255]);
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screens-probe");
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join(format!("{name}.png"));
    image::save_buffer(
        &path,
        &rgba,
        u32::from(b.width),
        u32::from(b.height),
        image::ColorType::Rgba8,
    )
    .expect("png");
    println!("SHOT {}", path.display());
}

/// Walk to the pipe cluster of YI1 (`n` frames), then run `RF_TAIL` (a
/// script) and print the colour-histogram jumps and shots.
#[test]
#[ignore = "needs the SMW ROM"]
fn smw_pipe_try() {
    let Some((mut st, mut sink)) = open() else {
        return;
    };
    to_yi1(&mut st, &mut sink);
    let n: u32 = std::env::var("RF_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1750);
    for f in 0..n {
        st.poke_ram(0x7E_1490, 0xFF);
        let mut p = btn("R+B");
        if f % 40 < 14 {
            p.push(SnesButton::A);
        }
        drop(frame(&mut st, &mut sink, &p));
    }
    if let Ok(dx) = std::env::var("RF_POKE_X") {
        let dx: u16 = dx.parse().unwrap();
        let x = (u16::from(st.peek_bus(0x7E_0094)) | u16::from(st.peek_bus(0x7E_0095)) << 8) + dx;
        st.poke_ram(0x7E_0094, (x & 0xFF) as u8);
        st.poke_ram(0x7E_0095, (x >> 8) as u8);
        run(&mut st, &mut sink, &[], 20);
    }
    let b = frame(&mut st, &mut sink, &[]);
    save_snes(&st, &b, "pipe-0");
    if let Ok(pokes) = std::env::var("RF_POKE") {
        for kv in pokes.split(',') {
            let (a, v) = kv.split_once('=').unwrap();
            st.poke_ram(
                0x7E_0000 + u32::from_str_radix(a, 16).unwrap(),
                u8::from_str_radix(v, 16).unwrap(),
            );
        }
    }
    let tail = std::env::var("RF_TAIL").unwrap_or_else(|_| "R+A25 w20 D30 w60".into());
    let mut prev = Some(histogram(&b));
    for (k, tok) in tail.split_whitespace().enumerate() {
        let (p, cnt) = if let Some(c) = tok.strip_prefix('w') {
            (Vec::new(), c.parse::<u32>().unwrap())
        } else {
            let s = tok.find(|c: char| c.is_ascii_digit()).unwrap();
            (btn(&tok[..s]), tok[s..].parse::<u32>().unwrap())
        };
        for _ in 0..cnt {
            st.poke_ram(0x7E_1490, 0xFF);
            let b = frame(&mut st, &mut sink, &p);
            let h = histogram(&b);
            if let Some(pv) = &prev {
                let d = hist_dist(pv, &h);
                if d > 0.2 {
                    println!("hist jump {d:.2} at {tok}");
                }
            }
            prev = Some(h);
        }
        let b = frame(&mut st, &mut sink, &[]);
        save_snes(&st, &b, &format!("pipe-{}", k + 1));
        mark(&st, tok);
    }
}

/// YI1 -> its pipe room: bytes constant in each, different between them.
#[test]
#[ignore = "needs the SMW ROM"]
fn smw_pipe_key() {
    let Some((mut st, mut sink)) = open() else {
        return;
    };
    to_yi1(&mut st, &mut sink);
    let map_like = constant_window(&mut st, &mut sink, &[], 12);
    for f in 0..1750u32 {
        st.poke_ram(0x7E_1490, 0xFF);
        let mut p = btn("R+B");
        if f % 40 < 14 {
            p.push(SnesButton::A);
        }
        drop(frame(&mut st, &mut sink, &p));
    }
    let x = (u16::from(st.peek_bus(0x7E_0094)) | u16::from(st.peek_bus(0x7E_0095)) << 8) + 100;
    st.poke_ram(0x7E_0094, (x & 0xFF) as u8);
    st.poke_ram(0x7E_0095, (x >> 8) as u8);
    run(&mut st, &mut sink, &[], 20);
    let main = constant_window(&mut st, &mut sink, &[], 12);
    st.poke_ram(0x7E_0071, 0x06);
    run(&mut st, &mut sink, &[], 260);
    let room = constant_window(&mut st, &mut sink, &[], 12);
    println!("start-of-level(frame 0) vs main vs room:");
    let mut shown = 0;
    for a in 0..main.len() {
        if let (Some(m), Some(r)) = (main[a], room[a]) {
            if m != r && a < 0x2000 && shown < 120 {
                println!(
                    "  7E{a:04X}: start={} main={m:02X} room={r:02X}",
                    map_like[a].map_or("--".to_string(), |v| format!("{v:02X}"))
                );
                shown += 1;
            }
        }
    }
}

struct Obs {
    model: rf_enhance::canvas::CanvasModel,
    spec: Option<retroforge::scene_key::SceneKeySpec>,
    last: Option<rf_enhance::scene_identity::SceneId>,
    log: Vec<(u64, rf_enhance::scene_identity::SceneId)>,
    /// The scroll read after the previous frame: what drew this one.
    prev: Option<[Option<rf_enhance::canvas::LayerScroll>; 4]>,
    key_seen: Option<Vec<u8>>,
}

impl Obs {
    fn new(profile: Option<&str>) -> Self {
        let spec = profile.map(|rel| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../profiles")
                .join(rel);
            let outcome = rf_profiles::load_file(&path).expect("loads");
            retroforge::scene_key::SceneKeySpec::from_profile(
                outcome.profile.scene.as_ref().expect("[scene]"),
            )
        });
        Self {
            model: rf_enhance::canvas::CanvasModel::new(),
            spec,
            last: None,
            log: Vec::new(),
            prev: None,
            key_seen: None,
        }
    }

    /// One frame the way the core thread sees it (BG1's scroll fed to the
    /// scene tracker, the profile's key read from the bus).
    fn step(
        &mut self,
        st: &mut EmuStepper,
        sink: &mut Sink,
        b: &[SnesButton],
    ) -> rf_core_api::FrameBundle {
        let bundle = frame(st, sink, b);
        let level = self.spec.as_ref().and_then(|k| k.read(|a| st.peek_bus(a)));
        if std::env::var("RF_KEYLOG").is_ok() && level != self.key_seen {
            println!(
                "f{} key {:02X?} hofs {}",
                st.frame_count(),
                level,
                bg1_hofs(st)
            );
            self.key_seen = level.clone();
            if st.frame_count() > 3000 {
                save_snes(st, &bundle, &format!("key-{}", st.frame_count()));
                println!(
                    "  mode {:02X} lives {:02X} x {:02X}{:02X} anim71 {:02X}",
                    st.peek_bus(0x7E_0100),
                    st.peek_bus(0x7E_0DBE),
                    st.peek_bus(0x7E_0095),
                    st.peek_bus(0x7E_0094),
                    st.peek_bus(0x7E_0071)
                );
            }
        }
        let views = st.bg_layer_views().expect("snes");
        let now = retroforge::core_thread::snes_layer_scrolls(st);
        let drawn = std::mem::replace(&mut self.prev, now);
        let feed = rf_enhance::canvas::FrameFeed::registers(drawn.as_ref())
            .with_scene_scroll(Some((views[0].hofs, views[0].vofs)))
            .with_scene_hints(rf_enhance::scene_tracker::SceneHints {
                level: level.as_deref(),
                axis_sections: self.spec.as_ref().and_then(|k| k.axis_sections),
            });
        let id = self.model.observe_frame(&bundle, &feed);
        if self.last != Some(id) {
            self.log.push((st.frame_count(), id));
            self.last = Some(id);
        }
        bundle
    }

    fn run(&mut self, st: &mut EmuStepper, sink: &mut Sink, b: &[SnesButton], n: u32) {
        for _ in 0..n {
            self.step(st, sink, b);
        }
    }

    fn script(&mut self, st: &mut EmuStepper, sink: &mut Sink, text: &str) {
        for tok in text.split_whitespace() {
            let (b, n) = if let Some(n) = tok.strip_prefix('w') {
                (Vec::new(), n.parse::<u32>().unwrap())
            } else {
                let s = tok.find(|c: char| c.is_ascii_digit()).unwrap();
                (btn(&tok[..s]), tok[s..].parse::<u32>().unwrap())
            };
            self.run(st, sink, &b, n);
        }
    }

    fn current(&self) -> rf_enhance::scene_identity::SceneId {
        self.last.expect("observed")
    }

    fn cells(&self, id: rf_enhance::scene_identity::SceneId) -> usize {
        self.model.scene_canvases(id.0).map_or(0, |c| {
            c.iter().map(|l| l.cells().iter().flatten().count()).sum()
        })
    }
}

/// Known cells of `before` whose value `after` still holds: `(known, same)`.
fn kept_cells(
    before: &[rf_enhance::stitcher::Canvas],
    after: &[rf_enhance::stitcher::Canvas],
) -> (usize, usize) {
    let (mut known, mut same) = (0, 0);
    for (b, a) in before.iter().zip(after) {
        let (ox, oy) = b.origin();
        for y in 0..b.height() as i64 {
            for x in 0..b.width() as i64 {
                if let Some(p) = b.get(ox + x, oy + y) {
                    known += 1;
                    same += usize::from(a.get(ox + x, oy + y) == Some(p));
                }
            }
        }
    }
    (known, same)
}

/// Map -> Yoshi's Island 1 -> its pipe room -> back, with the profile's key:
/// three scenes, the level's canvas untouched while the room is on screen, and
/// the SAME scene id when the player comes back out.
#[test]
#[ignore = "needs the SMW ROM"]
fn smw_scenes_pipe_room_and_back() {
    let (mut st, mut sink) = open().expect("SMW");
    let mut ob = Obs::new(Some("snes/super-mario-world/profile.toml"));
    ob.run(&mut st, &mut sink, &[], 200);
    for i in 0..10 {
        ob.script(&mut st, &mut sink, if i % 2 == 0 { "S6" } else { "A6" });
        ob.run(&mut st, &mut sink, &[], 100);
    }
    ob.run(&mut st, &mut sink, &[], 300);
    ob.script(&mut st, &mut sink, "A8 w400");
    let map = ob.current();
    println!("SCENE map {:016x}", map.0);
    ob.script(&mut st, &mut sink, "L8 w150 A8 w150");
    for f in 0..1750u32 {
        st.poke_ram(0x7E_1490, 0xFF);
        let mut p = btn("R+B");
        if f % 40 < 14 {
            p.push(SnesButton::A);
        }
        ob.step(&mut st, &mut sink, &p);
    }
    let level = ob.current();
    println!("SCENE YI1 {:016x}", level.0);
    // Stand Mario at the tall pipe, then send him down it: the pipe-entry
    // animation ($71 = 6) loads the room the level's own exit table names.
    let x = (u16::from(st.peek_bus(0x7E_0094)) | u16::from(st.peek_bus(0x7E_0095)) << 8) + 100;
    st.poke_ram(0x7E_0094, (x & 0xFF) as u8);
    st.poke_ram(0x7E_0095, (x >> 8) as u8);
    ob.run(&mut st, &mut sink, &[], 20);
    assert_eq!(ob.current(), level, "still in the level");
    let before: Vec<_> = ob
        .model
        .scene_canvases(level.0)
        .expect("level canvas")
        .to_vec();
    let cells_before = ob.cells(level);
    st.poke_ram(0x7E_0071, 0x06);
    ob.run(&mut st, &mut sink, &[], 300);
    let room = ob.current();
    println!(
        "SCENE pipe room {:016x}: {} cells; level canvas {} cells (was {cells_before})",
        room.0,
        ob.cells(room),
        ob.cells(level)
    );
    assert_ne!(map, level);
    assert_ne!(level, room, "the pipe room is its own scene");
    assert!(ob.cells(room) > 0);
    assert_eq!(
        ob.cells(level),
        cells_before,
        "the level canvas changed while its pipe room was on screen"
    );
    // The room's way out is a pipe at its far right behind two walls of
    // blocks: lift Mario over them (Y held, X poked) and let him walk in.
    let ground = u16::from(st.peek_bus(0x7E_0096)) | u16::from(st.peek_bus(0x7E_0097)) << 8;
    for _ in 0..120u32 {
        let x = (u16::from(st.peek_bus(0x7E_0094)) | u16::from(st.peek_bus(0x7E_0095)) << 8) + 2;
        st.poke_ram(0x7E_0094, (x & 0xFF) as u8);
        st.poke_ram(0x7E_0095, (x >> 8) as u8);
        let y = ground - 0x58;
        st.poke_ram(0x7E_0096, (y & 0xFF) as u8);
        st.poke_ram(0x7E_0097, (y >> 8) as u8);
        ob.step(&mut st, &mut sink, &btn("R"));
    }
    ob.script(&mut st, &mut sink, "R+B200 w200");
    let after = ob.current();
    let (known, same) = kept_cells(
        &before,
        ob.model.scene_canvases(level.0).expect("level canvas"),
    );
    println!("SCENE back in the level {:016x}; level cells {} -> {}; of the {known} cells known before the pipe {same} still hold", after.0, cells_before, ob.cells(level));
    println!(
        "scenes: {:?}",
        ob.log
            .iter()
            .map(|(f, i)| format!("f{f}:{:x}", i.0 & 0xFFFF))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        after, level,
        "coming out of the pipe is the same scene, so the same canvases"
    );
    assert!(ob.cells(level) >= cells_before);
    assert!(
        same * 100 >= known * 99,
        "{same} of {known} cells survived the round trip"
    );
    let leaving = ob
        .model
        .scene_edges()
        .iter()
        .find(|e| e.from == level.0 && e.to == room.0)
        .copied()
        .expect("an edge into the room");
    assert!(!leaving.continuous, "a pipe is not a continuous edge");
    assert!(
        ob.model.margin_neighbours(level).is_empty(),
        "a pipe room must not show in the level's margin"
    );
}
