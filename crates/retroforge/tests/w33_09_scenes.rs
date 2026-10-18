//! **W33-09: scene identity on the NES (Super Mario Bros. 3, Mega Man 2).**
//! ROM-gated (law 5): every test is `#[ignore]` and skips with a message when
//! the ROM is absent. Games run headless (`EmuStepper`), every frame fed to a
//! `CanvasModel` the way the core thread does it (the profile's scene key read
//! from the bus), with no canvas cache, so each run starts cold. The evidence
//! is written up in `docs/research/W33-09-scenes.md`.
//!
//! * `smb3_scenes_*`, `mm2_scenes_*`: the acceptance runs (title, map, level,
//!   another level; Air Man's horizontal section and the climb), with and
//!   without the profile.
//! * `raw_smb3`, `raw_mm2`: the fault: the pure per-frame hash is one value.
//! * `smb3_level_id`, `smb3_watch`, `mm2_stage_bytes`, `mm2_section_bytes`,
//!   `mm2_walk`: how the `[[scene.level_id]]` rows were found (RAM diffs
//!   between states; `RF_WATCH=hex,hex` logs changes of bytes).
//!
//! ```text
//! cargo test --release -p retroforge --test w33_09_scenes -- --ignored --nocapture <probe>
//! ```
//!
//! `RF_NES_SCRIPT` drives `script_shots`: whitespace separated tokens, `wN`
//! idle N frames, `<buttons>N` hold N frames (buttons: A B S(tart) E(select)
//! U D L R, joined with `+`), `shot:name` writes `target/screens-probe/<name>.png`.

use std::path::{Path, PathBuf};

use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreSink, FrameBundle, FrameBundleBuilder, InputFrame, PpuPixel};
use rf_enhance::canvas::{CanvasModel, FrameFeed};

pub const W: u16 = 256;
pub const H: u16 = 240;

fn rom_path(file: &str) -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join("nes").join(file);
    p.exists().then_some(p)
}

pub struct Sink {
    pub bundle: FrameBundleBuilder,
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

pub fn open(file: &str) -> Option<(EmuStepper, Sink)> {
    let Some(path) = rom_path(file) else {
        println!("SKIP: {file} not found");
        return None;
    };
    let raw = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let mut st = EmuStepper::open(&raw).expect("opens");
    st.set_event_mask(rf_core_api::EventMask::ALL);
    Some((
        st,
        Sink {
            bundle: FrameBundleBuilder::new(W, H),
        },
    ))
}

pub const BTN_A: u16 = 1 << 0;
pub const BTN_B: u16 = 1 << 1;
pub const BTN_SELECT: u16 = 1 << 2;
pub const BTN_START: u16 = 1 << 3;
pub const BTN_UP: u16 = 1 << 4;
pub const BTN_DOWN: u16 = 1 << 5;
pub const BTN_LEFT: u16 = 1 << 6;
pub const BTN_RIGHT: u16 = 1 << 7;

fn pad(mask: u16) -> InputFrame {
    let mut f = InputFrame::empty();
    f.ports[0] = mask;
    f
}

/// What the core thread does with every frame, run on each probe frame:
/// the profile's scene key is read from the bus and the canvas model fed.
struct Observer {
    model: CanvasModel,
    spec: Option<retroforge::scene_key::SceneKeySpec>,
    last: Option<rf_enhance::scene_identity::SceneId>,
    /// `(frame, scene)` at every change of scene.
    log: Vec<(u64, rf_enhance::scene_identity::SceneId)>,
    /// Time spent in `observe_frame`, and the frames it took over.
    spent: std::time::Duration,
    frames: u64,
}

thread_local! {
    static OBSERVER: std::cell::RefCell<Option<Observer>> = const { std::cell::RefCell::new(None) };
    static HORIZ: std::cell::RefCell<Option<Vec<Option<u8>>>> = const { std::cell::RefCell::new(None) };
    /// Bytes constant on every frame since `track_begin` (NES RAM probe).
    static TRACK: std::cell::RefCell<Option<Vec<Option<u8>>>> = const { std::cell::RefCell::new(None) };
    /// Addresses whose changes `frame` logs (RF_WATCH=hex,hex).
    static WATCH: std::cell::RefCell<(Vec<u32>, Vec<u8>)> = const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
}

fn watch_from_env() {
    if let Ok(v) = std::env::var("RF_WATCH") {
        let a = v
            .split(',')
            .map(|a| u32::from_str_radix(a, 16).unwrap())
            .collect();
        WATCH.with(|w| *w.borrow_mut() = (a, Vec::new()));
    }
}

/// One frame; returns its bundle.
pub fn frame(st: &mut EmuStepper, sink: &mut Sink, mask: u16) -> FrameBundle {
    st.latch_and_advance_frame(pad(mask), sink);
    let bundle = sink.bundle.take(st.frame_count());
    OBSERVER.with(|o| {
        if let Some(ob) = o.borrow_mut().as_mut() {
            let level = ob.spec.as_ref().and_then(|k| k.read(|a| st.peek_bus(a)));
            let feed = FrameFeed::bands().with_scene_hints(rf_enhance::scene_tracker::SceneHints {
                level: level.as_deref(),
                axis_sections: ob.spec.as_ref().and_then(|k| k.axis_sections),
            });
            let t0 = std::time::Instant::now();
            let id = ob.model.observe_frame(&bundle, &feed);
            ob.spent += t0.elapsed();
            ob.frames += 1;
            if ob.last != Some(id) {
                ob.log.push((st.frame_count(), id));
                ob.last = Some(id);
            }
        }
    });
    TRACK.with(|t| {
        if let Some(c) = t.borrow_mut().as_mut() {
            for (slot, v) in c.iter_mut().zip(watched(st, false)) {
                if *slot != Some(v) {
                    *slot = None;
                }
            }
        }
    });
    WATCH.with(|w| {
        let mut w = w.borrow_mut();
        if w.0.is_empty() {
            return;
        }
        let now: Vec<u8> = w.0.iter().map(|&a| st.peek_bus(a)).collect();
        if now != w.1 {
            println!("f{} {:02X?}", st.frame_count(), now);
            w.1 = now;
        }
    });
    bundle
}

/// Start feeding every probe frame to a fresh canvas model, with the scene
/// table of `profile` (a path under `profiles/`), or none.
pub fn observe_with(profile: Option<&str>) {
    let spec = profile.map(|rel| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles")
            .join(rel);
        let outcome = rf_profiles::load_file(&path).expect("the profile loads");
        retroforge::scene_key::SceneKeySpec::from_profile(
            outcome.profile.scene.as_ref().expect("[scene]"),
        )
    });
    OBSERVER.with(|o| {
        *o.borrow_mut() = Some(Observer {
            model: CanvasModel::new(),
            spec,
            last: None,
            log: Vec::new(),
            spent: std::time::Duration::ZERO,
            frames: 0,
        });
    });
}

/// Mean microseconds one `observe_frame` took (scene tracking AND stitching).
pub fn mean_observe_micros() -> f64 {
    OBSERVER.with(|o| {
        let o = o.borrow();
        let ob = o.as_ref().expect("observing");
        ob.spent.as_secs_f64() * 1e6 / ob.frames.max(1) as f64
    })
}

/// The scenes seen, in order, with the frame each began.
pub fn observed() -> Vec<(u64, rf_enhance::scene_identity::SceneId)> {
    OBSERVER.with(|o| {
        o.borrow()
            .as_ref()
            .map(|ob| ob.log.clone())
            .unwrap_or_default()
    })
}

/// Cells of layer 0 of `scene` the model knows.
pub fn cell_count(scene: rf_enhance::scene_identity::SceneId) -> usize {
    OBSERVER.with(|o| {
        let o = o.borrow();
        let m = &o.as_ref().expect("observing").model;
        m.scene_canvases(scene.0)
            .map_or(0, |c| c[0].cells().iter().flatten().count())
    })
}

pub fn run(st: &mut EmuStepper, sink: &mut Sink, mask: u16, n: u32) {
    for _ in 0..n {
        drop(frame(st, sink, mask));
    }
}

pub fn save_bundle(b: &FrameBundle, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screens-probe");
    std::fs::create_dir_all(&dir).expect("dir");
    let mut rgba = Vec::with_capacity(b.video.len() * 4);
    for p in &b.video {
        let c = rf_renderer::resolve_index(p.palette_index, None);
        rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
    }
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

fn mask_of(tok: &str) -> u16 {
    tok.split('+').fold(0, |m, c| {
        m | match c {
            "A" => BTN_A,
            "B" => BTN_B,
            "S" => BTN_START,
            "E" => BTN_SELECT,
            "U" => BTN_UP,
            "D" => BTN_DOWN,
            "L" => BTN_LEFT,
            "R" => BTN_RIGHT,
            _ => 0,
        }
    })
}

/// Run a script token string; returns the last bundle.
pub fn script(st: &mut EmuStepper, sink: &mut Sink, text: &str) -> Option<FrameBundle> {
    let mut last = None;
    for tok in text.split_whitespace() {
        if let Some(name) = tok.strip_prefix("shot:") {
            if let Some(b) = last.as_ref() {
                save_bundle(b, name);
            }
            continue;
        }
        if let Some(n) = tok.strip_prefix('w') {
            let n: u32 = n.parse().expect("wN");
            for _ in 0..n {
                last = Some(frame(st, sink, 0));
            }
            continue;
        }
        let split = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
        let (b, n) = tok.split_at(split);
        let (m, n) = (mask_of(b), n.parse::<u32>().expect("N"));
        for _ in 0..n {
            last = Some(frame(st, sink, m));
        }
    }
    last
}

const SMB3_TO_LEVEL: &str = "w300 S6 w120 S6 w400 R8 w60 U8 w60 A8 w300";

/// Root-cause probe: the pure per-frame hash (no continuity) over the same
/// script, and the biggest per-frame world jump the tracker saw.
fn raw_ids(file: &str, text: &str) {
    let Some((mut st, mut sink)) = open(file) else {
        return;
    };
    let mut tracker = rf_enhance::scene_tracker::SceneTracker::new();
    let mut raw = std::collections::BTreeSet::new();
    let (mut max_jump, mut prev) = (0i64, None::<(i64, i64)>);
    let mut tracker_time = std::time::Duration::ZERO;
    let mut f = 0u32;
    for tok in text.split_whitespace() {
        let (m, n) = if let Some(n) = tok.strip_prefix('w') {
            (0, n.parse::<u32>().unwrap())
        } else {
            let s = tok.find(|c: char| c.is_ascii_digit()).unwrap();
            (mask_of(&tok[..s]), tok[s..].parse::<u32>().unwrap())
        };
        for _ in 0..n {
            let b = frame(&mut st, &mut sink, m);
            raw.insert(
                rf_enhance::scene_identity::compute_scene_id(&b.video, b.width, b.height, &[]).0,
            );
            let t0 = std::time::Instant::now();
            tracker.observe_frame(&b.video, b.width, b.height, &b.events, &[]);
            tracker_time += t0.elapsed();
            let w = tracker.last_world_position();
            if let (Some(p), Some(c)) = (prev, w) {
                let j = (c.0 - p.0).abs().max((c.1 - p.1).abs());
                if j > 8 {
                    println!("f{f} jump {j} ({:?}->{:?})", p, c);
                }
                max_jump = max_jump.max(j);
            }
            prev = w;
            f += 1;
        }
    }
    println!(
        "{file}: raw per-frame hash distinct {} ; max world jump {max_jump}",
        raw.len()
    );
    println!(
        "SceneTracker alone: mean {:.0} us a frame over {f} frames",
        tracker_time.as_secs_f64() * 1e6 / f64::from(f.max(1))
    );
}

#[test]
#[ignore = "needs SMB3"]
fn raw_smb3() {
    raw_ids("Super Mario Bros. 3 (USA).zip", SMB3_TO_LEVEL);
}

#[test]
#[ignore = "needs MM2"]
fn raw_mm2() {
    raw_ids(
        "Mega Man 2 (USA).zip",
        "w400 S6 w120 S6 w300 S6 w300 U8 w20 S6 w800 R200",
    );
}

#[test]
#[ignore = "needs a NES ROM; RF_NES_ROM + RF_NES_SCRIPT"]
fn script_shots() {
    let file =
        std::env::var("RF_NES_ROM").unwrap_or_else(|_| "Super Mario Bros. 3 (USA).zip".into());
    let Some((mut st, mut sink)) = open(&file) else {
        return;
    };
    let text = std::env::var("RF_NES_SCRIPT").unwrap_or_default();
    script(&mut st, &mut sink, &text);
    println!("frames {}", st.frame_count());
}

// ---- RAM diff tool: bytes constant over a window in each state, compared
// across states. A level-id candidate is constant inside every state and
// differs between the states the scene key must tell apart.

/// Bytes a probe watches: NES work RAM + PRG-RAM, SNES `$7E0000..$7EFFFF`.
fn watched(st: &EmuStepper, snes: bool) -> Vec<u8> {
    if snes {
        (0..0x10000u32)
            .map(|a| st.peek_bus(0x7E_0000 + a))
            .collect()
    } else {
        (0..0x800u32)
            .chain(0x6000..0x8000)
            .map(|a| st.peek_bus(a))
            .collect()
    }
}

fn addr_name(i: usize, snes: bool) -> String {
    if snes {
        format!("7E{i:04X}")
    } else if i < 0x800 {
        format!("{i:04X}")
    } else {
        format!("{:04X}", 0x6000 + i - 0x800)
    }
}

/// Run `n` frames holding `mask`; bytes that never changed are `Some`.
fn constant_window(
    st: &mut EmuStepper,
    sink: &mut Sink,
    mask: u16,
    n: u32,
    snes: bool,
) -> Vec<Option<u8>> {
    let mut c: Vec<Option<u8>> = watched(st, snes).into_iter().map(Some).collect();
    for _ in 0..n {
        drop(frame(st, sink, mask));
        for (slot, v) in c.iter_mut().zip(watched(st, snes)) {
            if *slot != Some(v) {
                *slot = None;
            }
        }
    }
    c
}

/// Print every byte constant in all `states` whose values are not all equal,
/// marking which pairs it separates. `need` lists pairs (by index) that must
/// differ to be printed.
fn print_diff(
    names: &[&str],
    states: &[Vec<Option<u8>>],
    need: &[(usize, usize)],
    snes: bool,
    limit: usize,
) {
    let mut shown = 0;
    for i in 0..states[0].len() {
        let vals: Vec<Option<u8>> = states.iter().map(|s| s[i]).collect();
        if vals.iter().any(Option::is_none) {
            continue;
        }
        if need.iter().all(|&(a, b)| vals[a] != vals[b]) {
            let row: Vec<String> = names
                .iter()
                .zip(&vals)
                .map(|(n, v)| format!("{n}={:02X}", v.unwrap()))
                .collect();
            println!("  {}: {}", addr_name(i, snes), row.join(" "));
            shown += 1;
            if shown >= limit {
                println!("  ... (limit)");
                return;
            }
        }
    }
    println!("  {shown} byte(s)");
}

/// Walk SMB3 1-1 the way the scout does (Mario held at Y 0x28, X poked +4 a
/// frame, neutral pad).
fn smb3_walk_through(st: &mut EmuStepper, sink: &mut Sink, poke_frames: u32) {
    for _ in 0..poke_frames {
        let x = (u32::from(st.peek_bus(0x90)) | u32::from(st.peek_bus(0x75)) << 8) + 4;
        st.poke_bus(0x00A2, 0x28);
        st.poke_bus(0x0090, (x & 0xFF) as u8);
        st.poke_bus(0x0075, (x >> 8) as u8);
        drop(frame(st, sink, 0));
    }
}

/// From the first SMB3 level's start to its Course Clear: walk the way the
/// scout does, then lower Mario under the end card and jump into it (Y 0x7a
/// at X 0xa86, then the A button).
fn smb3_clear_1_1(st: &mut EmuStepper, sink: &mut Sink) {
    smb3_walk_through(st, sink, 560);
    smb3_clear_1_1_tail(st, sink);
}

/// The second half of [`smb3_clear_1_1`]: the walk has reached the end.
fn smb3_clear_1_1_tail(st: &mut EmuStepper, sink: &mut Sink) {
    for f in 0..320u32 {
        let x = u32::from(st.peek_bus(0x90)) | u32::from(st.peek_bus(0x75)) << 8;
        if f < 260 {
            let nx = if x < 0x0A86 { x + 2 } else { x };
            st.poke_bus(0x0090, (nx & 0xFF) as u8);
            st.poke_bus(0x0075, (nx >> 8) as u8);
            st.poke_bus(0x00A2, if x < 0x0A60 { 0x28 } else { 0x7A });
        }
        drop(frame(
            st,
            sink,
            if (262..300).contains(&f) { BTN_A } else { 0 },
        ));
    }
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_level_id() {
    let Some((mut st, mut sink)) = open("Super Mario Bros. 3 (USA).zip") else {
        return;
    };
    script(&mut st, &mut sink, "w250");
    let title = constant_window(&mut st, &mut sink, 0, 20, false);
    script(&mut st, &mut sink, "S6 w120 S6 w400");
    let map_a = constant_window(&mut st, &mut sink, 0, 20, false);
    script(&mut st, &mut sink, "R8 w60 U8 w60 A8 w300");
    let lvl1 = constant_window(&mut st, &mut sink, 0, 20, false);
    smb3_clear_1_1(&mut st, &mut sink);
    script(&mut st, &mut sink, "w900");
    let map_b = constant_window(&mut st, &mut sink, 0, 20, false);
    script(&mut st, &mut sink, "R8 w60 R8 w60 A8 w300");
    save_bundle(&frame(&mut st, &mut sink, 0), "smb3-lvl2");
    let lvl2 = constant_window(&mut st, &mut sink, 0, 20, false);
    let names = ["title", "mapA", "lvl1", "mapB", "lvl2"];
    let states = [title, map_a, lvl1, map_b, lvl2];
    println!("level vs level vs map (lvl1!=lvl2, lvl1!=mapA, lvl2!=mapB, lvl1!=mapB, lvl2!=mapA):");
    print_diff(
        &names,
        &states,
        &[(2, 4), (2, 1), (4, 3), (2, 3), (4, 1)],
        false,
        80,
    );
    println!("same on both maps (mapA==mapB), differs from both levels:");
    let n = states[0].len();
    for a in 0..n {
        let v: Vec<Option<u8>> = states.iter().map(|s| s[a]).collect();
        if v.iter().all(Option::is_some)
            && v[1] == v[3]
            && v[1] != v[2]
            && v[1] != v[4]
            && v[0] != v[2]
        {
            println!(
                "  {}: title={:02X} map={:02X} lvl1={:02X} lvl2={:02X}",
                addr_name(a, false),
                v[0].unwrap(),
                v[1].unwrap(),
                v[2].unwrap(),
                v[4].unwrap()
            );
        }
    }
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_watch() {
    let Some((mut st, mut sink)) = open("Super Mario Bros. 3 (USA).zip") else {
        return;
    };
    watch_from_env();
    script(
        &mut st,
        &mut sink,
        "w250 S6 w120 S6 w400 R8 w60 U8 w60 A8 w300",
    );
    smb3_clear_1_1(&mut st, &mut sink);
    script(&mut st, &mut sink, "w900 R8 w60 R8 w60 A8 w300 w200");
}

fn track_begin(st: &EmuStepper) {
    TRACK.with(|t| *t.borrow_mut() = Some(watched(st, false).into_iter().map(Some).collect()));
}

fn track_end() -> Vec<Option<u8>> {
    TRACK.with(|t| t.borrow_mut().take().expect("tracking"))
}

const MM2: &str = "Mega Man 2 (USA).zip";
const MM2_TO_AIRMAN: &str = "w400 S6 w120 S6 w300 S6 w300 U8 w20 S6 w800";

/// One frame of the MM2 walk: Right held, Mega Man's Y held high (the
/// scout's recipe) while `hold` is true.
fn mm2_step(st: &mut EmuStepper, sink: &mut Sink, hold: bool) -> FrameBundle {
    if hold {
        st.poke_bus(0x04A0, 0x20);
    }
    if std::env::var("RF_GOD").is_ok() {
        st.poke_bus(0x06C0, 0x1C);
    }
    let mask = std::env::var("RF_PAD").map_or(BTN_RIGHT, |t| mask_of(&t));
    frame(st, sink, mask)
}

#[test]
#[ignore = "needs MM2"]
fn mm2_walk() {
    let Some((mut st, mut sink)) = open(MM2) else {
        return;
    };
    watch_from_env();
    script(&mut st, &mut sink, MM2_TO_AIRMAN);
    let mut tracker = rf_enhance::scene_tracker::SceneTracker::new();
    let n: u32 = std::env::var("RF_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4000);
    let hold_until: u32 = std::env::var("RF_HOLD_UNTIL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(n);
    let mut last_y = None;
    for f in 0..n {
        let b = mm2_step(&mut st, &mut sink, f < hold_until);
        tracker.observe_frame(&b.video, b.width, b.height, &b.events, &[]);
        let w = tracker.last_world_position();
        if f % 200 == 0 || w.map(|p| p.1) != last_y {
            println!(
                "W f{f} world {w:?} cam={:#04x}{:02x} x={:#04x} y={:#04x}",
                st.peek_bus(0x20),
                st.peek_bus(0x1F),
                st.peek_bus(0x460),
                st.peek_bus(0x4A0)
            );
        }
        last_y = w.map(|p| p.1);
        if f % 500 == 499 || f + 1 == n {
            save_bundle(&b, &format!("mm2w-{f}"));
        }
    }
}

#[test]
#[ignore = "needs MM2"]
fn mm2_section_bytes() {
    let Some((mut st, mut sink)) = open(MM2) else {
        return;
    };
    script(&mut st, &mut sink, MM2_TO_AIRMAN);
    for f in 0..1900u32 {
        drop(mm2_step(&mut st, &mut sink, f < 2150));
    }
    track_begin(&st);
    for f in 1900..2020u32 {
        drop(mm2_step(&mut st, &mut sink, f < 2150));
    }
    let h = track_end();
    for f in 2020..2205u32 {
        drop(mm2_step(&mut st, &mut sink, f < 2150));
    }
    track_begin(&st);
    for f in 2205..2245u32 {
        drop(mm2_step(&mut st, &mut sink, f < 2150));
    }
    let v = track_end();
    let n = h.len();
    for a in 0..n {
        if let (Some(x), Some(y)) = (h[a], v[a]) {
            if x != y && !(0x100..0x400).contains(&a) {
                println!("  {}: horiz={x:02X} vert={y:02X}", addr_name(a, false));
            }
        }
    }
}

/// Bytes constant through the walk of one MM2 stage (frames 600..1400 of it).
fn mm2_stage_constants(pick: &str) -> Vec<Option<u8>> {
    let (mut st, mut sink) = open(MM2).expect("rom");
    script(&mut st, &mut sink, "w400 S6 w120 S6 w300 S6 w300");
    script(&mut st, &mut sink, pick);
    script(&mut st, &mut sink, "S6 w800");
    for f in 0..600u32 {
        let _ = f;
        st.poke_bus(0x06C0, 0x1C);
        drop(frame(&mut st, &mut sink, BTN_RIGHT));
    }
    track_begin(&st);
    for _ in 0..800u32 {
        st.poke_bus(0x06C0, 0x1C);
        drop(frame(&mut st, &mut sink, BTN_RIGHT));
    }
    track_end()
}

#[test]
#[ignore = "needs MM2"]
fn mm2_stage_bytes() {
    let picks = [
        ("air", "U8 w20"),
        ("wood", "L8 w20 U8 w20"),
        ("bubble", "D8 w20"),
        ("metal", "R8 w20"),
    ];
    let states: Vec<Vec<Option<u8>>> = picks.iter().map(|(_, p)| mm2_stage_constants(p)).collect();
    let names: Vec<&str> = picks.iter().map(|p| p.0).collect();
    println!("constant per stage, all four stages different:");
    let need: Vec<(usize, usize)> = (0..4)
        .flat_map(|a| (a + 1..4).map(move |b| (a, b)))
        .collect();
    print_diff(&names, &states, &need, false, 40);
    println!("constant per stage, first two differ:");
    print_diff(&names, &states, &[(0, 1)], false, 60);
}

/// The scene of the last frame.
pub fn current() -> rf_enhance::scene_identity::SceneId {
    OBSERVER.with(|o| {
        o.borrow()
            .as_ref()
            .and_then(|ob| ob.last)
            .expect("observing")
    })
}

pub fn edges() -> Vec<rf_enhance::scene_tracker::SceneEdge> {
    OBSERVER.with(|o| {
        o.borrow()
            .as_ref()
            .expect("observing")
            .model
            .scene_edges()
            .to_vec()
    })
}

fn show(tag: &str) -> rf_enhance::scene_identity::SceneId {
    let id = current();
    println!("SCENE {tag}: {:016x}", id.0);
    id
}

/// Title, map, level, another level: with the profile's level id, and
/// without (the picture alone).
fn smb3_scene_run(profile: Option<&str>) -> [rf_enhance::scene_identity::SceneId; 5] {
    let (mut st, mut sink) = open("Super Mario Bros. 3 (USA).zip").expect("SMB3");
    observe_with(profile);
    script(&mut st, &mut sink, "w250");
    let title = show("title");
    script(&mut st, &mut sink, "S6 w120 S6 w400");
    let map = show("map");
    script(&mut st, &mut sink, "R8 w60 U8 w60 A8 w300");
    let lvl1 = show("1-1 start");
    let before = cell_count(lvl1);
    smb3_walk_through(&mut st, &mut sink, 560);
    let after = cell_count(current());
    println!(
        "1-1 canvas: {before} -> {after} cells; scene {:016x}",
        current().0
    );
    smb3_clear_1_1_tail(&mut st, &mut sink);
    script(&mut st, &mut sink, "w900");
    let map_b = show("map after 1-1");
    script(&mut st, &mut sink, "R8 w60 R8 w60 A8 w300");
    let lvl2 = show("1-2 start");
    println!(
        "observe_frame mean {:.0} us over the session",
        mean_observe_micros()
    );
    println!(
        "scenes in order: {:?}",
        observed()
            .iter()
            .map(|(f, i)| format!("f{f}:{:x}", i.0 & 0xFFFF))
            .collect::<Vec<_>>()
    );
    [title, map, lvl1, map_b, lvl2]
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_scenes_with_the_profile() {
    let [title, map, lvl1, map_b, lvl2] =
        smb3_scene_run(Some("nes/super-mario-bros-3/profile.toml"));
    let all = [title, map, lvl1, lvl2];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a, b, "two screens share a scene id");
        }
    }
    assert_ne!(map_b, lvl1);
    assert_ne!(map_b, lvl2);
    assert_ne!(lvl1.0, rf_enhance::scene_tracker::SETTLING.0);
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_scenes_without_a_profile() {
    let [title, map, lvl1, _map_b, lvl2] = smb3_scene_run(None);
    // The picture alone tells the title, the map and a level apart.
    assert_ne!(title, map);
    assert_ne!(map, lvl1);
    assert_ne!(title, lvl1);
    println!(
        "1-1 vs 1-2 without a profile: {}",
        if lvl1 == lvl2 {
            "same scene"
        } else {
            "different scenes"
        }
    );
}

/// Mega Man 2: title, menu, stage select, Air Man's horizontal section, then
/// the climb. Returns (select, horizontal, vertical) scenes and the horizontal
/// canvas's cell count just before and after the climb.
fn mm2_scene_run(profile: Option<&str>) -> [rf_enhance::scene_identity::SceneId; 3] {
    let (mut st, mut sink) = open(MM2).expect("MM2");
    observe_with(profile);
    script(&mut st, &mut sink, "w380");
    show("title");
    script(&mut st, &mut sink, "S6 w120 S6 w300");
    show("menu");
    script(&mut st, &mut sink, "S6 w300 U8 w20");
    let select = show("stage select");
    script(&mut st, &mut sink, "S6 w800");
    let mut horizontal = None;
    let mut cells_when_left = None;
    for f in 0..2500u32 {
        st.poke_bus(0x06C0, 0x1C);
        drop(mm2_step(&mut st, &mut sink, f < 2150));
        if f == 2000 {
            horizontal = Some(show("horizontal section"));
        }
        if let Some(h) = horizontal {
            if cells_when_left.is_none()
                && current() != h
                && current() != rf_enhance::scene_tracker::SETTLING
            {
                cells_when_left = Some(cell_count(h));
            }
        }
    }
    let vertical = show("after the climb");
    let horizontal = horizontal.expect("reached");
    let cells_when_left = cells_when_left.expect("left the section");
    println!(
        "horizontal canvas {cells_when_left} cells when left, {} after; vertical canvas {} cells",
        cell_count(horizontal),
        cell_count(vertical)
    );
    println!("edges: {:?}", edges().last());
    assert_eq!(
        cell_count(horizontal),
        cells_when_left,
        "the first section's canvas changed after leaving it"
    );
    [select, horizontal, vertical]
}

#[test]
#[ignore = "needs MM2"]
fn mm2_scenes_with_the_profile() {
    let [select, horizontal, vertical] = mm2_scene_run(Some("nes/mega-man-2/profile.toml"));
    assert_ne!(select, horizontal);
    assert_ne!(horizontal, vertical, "the climb is a scene of its own");
    let join = edges()
        .into_iter()
        .find(|e| e.from == horizontal.0 && e.to == vertical.0)
        .expect("an edge joins the two sections");
    assert!(
        join.continuous,
        "the picture flowed from the horizontal section into the climb: {join:?}"
    );
}

#[test]
#[ignore = "needs MM2"]
fn mm2_scenes_without_a_profile() {
    let [select, horizontal, vertical] = mm2_scene_run(None);
    assert_ne!(select, horizontal);
    assert_ne!(horizontal, vertical, "the axis rule needs no profile");
}
