//! **W32-07: the level scout on Super Mario World (USA), Yoshi's Island 1.**
//!
//! ROM-gated (law 5: no ROM bytes in git): every test here is `#[ignore]`
//! and skips with a message when the ROM is absent.
//!
//! * `observe_camera` finds, by observation, the work-RAM words that hold
//!   BG1's X scroll and the player's X position (the evidence is written
//!   into `docs/research/W32-07-scout-smw.md`).
//! * `scout_strategies` tries the scout recipes on a snapshot of the live
//!   machine (writing the camera, moving the player's X word, and the
//!   profile's recipe) and compares what each stitched with a real walk.
//! * `restore_fidelity` proves a second machine restored from a snapshot
//!   draws the same frames as the live one.
//!
//! ```text
//! cargo test --release -p retroforge --test w32_07_scout_smw -- --ignored --nocapture observe
//! ```

use std::path::{Path, PathBuf};

use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreSink, FrameBundleBuilder, InputFrame, PpuPixel};
use rf_input::SnesButton;

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

fn word(st: &EmuStepper, off: u32) -> u16 {
    u16::from(st.peek_bus(0x7E_0000 + off)) | (u16::from(st.peek_bus(0x7E_0000 + off + 1)) << 8)
}

fn bg1_hofs(st: &EmuStepper) -> u16 {
    let v = st.bg_layer_views().expect("snes");
    v[0].hofs
}

#[test]
#[ignore = "needs the SMW ROM"]
fn observe_camera() {
    let Some((mut st, mut sink)) = open() else {
        return;
    };
    reach_yi1(&mut st, &mut sink);
    println!(
        "in level? tick14={:#04x} bg1 hofs={}",
        st.peek_bus(0x7E_0014),
        bg1_hofs(&st)
    );
    // Walk right with a hop now and then; sample the low-RAM words each
    // frame, and BG1HOFS from the PPU.
    const LOW: usize = 0x2000;
    let mut hofs: Vec<u16> = Vec::new();
    let mut ram: Vec<Vec<u8>> = Vec::new();
    for f in 0..600u32 {
        let jump = (f / 18) % 2 == 0;
        let keys: &[SnesButton] = if jump {
            &[SnesButton::Right, SnesButton::B]
        } else {
            &[SnesButton::Right]
        };
        run(&mut st, &mut sink, keys, 1);
        hofs.push(bg1_hofs(&st));
        ram.push(
            (0..LOW as u32)
                .map(|o| st.peek_bus(0x7E_0000 + o))
                .collect(),
        );
    }
    let moved = hofs.windows(2).filter(|w| w[0] != w[1]).count();
    println!(
        "frames {} hofs {}..{} moving-frames {moved}",
        hofs.len(),
        hofs.first().unwrap(),
        hofs.last().unwrap()
    );
    // Direct correlation: every even-or-odd offset's little-endian word
    // against BG1HOFS, same frame and one frame earlier.
    let w16 = |f: usize, o: usize| u16::from(ram[f][o]) | (u16::from(ram[f][o + 1]) << 8);
    let mut scored: Vec<(usize, usize, usize, usize)> = Vec::new();
    for o in 0..LOW - 1 {
        let (mut same, mut late, mut distinct) = (0usize, 0usize, std::collections::HashSet::new());
        for f in 1..hofs.len() {
            let w = w16(f, o);
            distinct.insert(w);
            if w == hofs[f] {
                same += 1;
            }
            if w == hofs[f - 1] {
                late += 1;
            }
        }
        scored.push((same.max(late), same, late, distinct.len()));
        let _ = o;
    }
    let mut ranked: Vec<(usize, (usize, usize, usize, usize))> =
        scored.into_iter().enumerate().collect();
    ranked.sort_by_key(|(_, s)| std::cmp::Reverse((s.0, s.3)));
    println!("--- words matching BG1HOFS (offset, best, same-frame, one-frame-late, distinct) ---");
    for (o, s) in ranked.iter().take(8) {
        println!(
            "  {o:#06x}  best {} same {} late {} distinct {}",
            s.0, s.1, s.2, s.3
        );
    }
    // Player X: a word that moves right with the camera's delta while the
    // camera moves, and sits 0..255 to the right of the camera edge (the
    // player's screen column), for every frame of the walk.
    println!("--- words that stay within [camera, camera+255] and move with it ---");
    let mut cand: Vec<(usize, usize, usize)> = Vec::new();
    for o in 0..LOW - 1 {
        let (mut inside, mut tracks) = (0usize, 0usize);
        for f in 1..hofs.len() {
            let w = w16(f, o);
            let rel = i32::from(w) - i32::from(hofs[f]);
            if (0..=255).contains(&rel) {
                inside += 1;
            }
            let dw = i32::from(w) - i32::from(w16(f - 1, o));
            let dh = i32::from(hofs[f]) - i32::from(hofs[f - 1]);
            if dh > 0 && dw > 0 && dw.abs_diff(dh) <= 2 {
                tracks += 1;
            }
        }
        if inside * 100 >= (hofs.len() - 1) * 95 && tracks >= 20 {
            cand.push((o, inside, tracks));
        }
    }
    cand.sort_by_key(|c| std::cmp::Reverse(c.2));
    for (o, inside, tracks) in cand.iter().take(8) {
        println!(
            "  {o:#06x}  inside {inside}/{} tracks {tracks}",
            hofs.len() - 1
        );
    }
    // Timing: BG1HOFS at frame f against the candidate word at frame f+k.
    for (name, off) in [("$1A", 0x1Ausize), ("$1462", 0x1462), ("$D1", 0xD1)] {
        let mut line = format!("lag table {name}:");
        for k in -3i32..=3 {
            let mut hit = 0;
            let mut n = 0;
            for (f, &hofs_f) in hofs.iter().enumerate().take(hofs.len() - 4).skip(4) {
                let g = (f as i32 + k) as usize;
                n += 1;
                if w16(g, off) == hofs_f {
                    hit += 1;
                }
            }
            line += &format!("  word@f{k:+}: {hit}/{n}");
        }
        println!("{line}");
    }
    // The profile's documented guesses, for comparison.
    for (name, off) in [("camera $1A", 0x1Au32), ("player_x $D1", 0xD1)] {
        println!(
            "{name}: last frame word {:#06x}; bg1 hofs {}",
            word(&st, off),
            bg1_hofs(&st)
        );
    }
    // A few frames of raw trace for the note.
    for f in (0..600).step_by(60) {
        println!(
            "  f{f:03} hofs {:4} ram[$1A] {:4} ram[$D1] {:4} ram[$1462] {:4}",
            hofs[f],
            w16(f, 0x1A),
            w16(f, 0xD1),
            w16(f, 0x1462.min(LOW - 2))
        );
    }
}

// ----------------------------------------------------------- scout runs

use retroforge::core_thread::snes_layer_scrolls;
use retroforge::scout::{Scout, ScoutDir, ScoutPlan, ScoutStatus, ScoutStrategy};
use rf_enhance::canvas::{CanvasModel, FrameFeed, LayerScroll};
use rf_enhance::scene_identity::SceneId;

#[path = "../../rf-harness/tests/smw_drive/mod.rs"]
mod smw_drive;

type Scrolls = Option<[Option<LayerScroll>; 4]>;

fn plan(strategy: ScoutStrategy, player: &[u32], camera: &[u32], step: u16) -> ScoutPlan {
    ScoutPlan {
        strategy,
        camera: camera.to_vec(),
        player_x: player.to_vec(),
        player_x_hi: Vec::new(),
        camera_y: Vec::new(),
        player_y: Vec::new(),
        player_y_hi: Vec::new(),
        step_px: step,
        frame_budget: 1500,
        stall_frames: 60,
        settle_frames: 0,
        dirs: vec![ScoutDir::Right, ScoutDir::Left],
        hold: Vec::new(),
        start_when: Vec::new(),
        pin: false,
        scene_key: None,
        reach_px: 0,
    }
}

/// The plan the SMW profile's `[scout]` table gives.
fn profile_plan() -> ScoutPlan {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/snes/super-mario-world/profile.toml");
    let outcome = rf_profiles::load_file(&path).expect("profile loads");
    ScoutPlan::from_profile(outcome.profile.scout.as_ref().expect("[scout]"))
}

/// A live frame as the core thread stitches it: step the machine, stitch per
/// layer at the scroll read after the PREVIOUS frame (what drew this one).
fn live_frame(
    st: &mut EmuStepper,
    sink: &mut Sink,
    acc: &mut CanvasModel,
    prev: &mut Scrolls,
    keys: &[SnesButton],
) {
    st.latch_and_advance_frame(pad(keys), sink);
    let bundle = sink.bundle.take(st.frame_count());
    let now = snes_layer_scrolls(st);
    let drawn_with = std::mem::replace(prev, now);
    acc.observe_frame_in(
        &bundle,
        SceneId(1),
        &FrameFeed::registers(drawn_with.as_ref()),
    );
}

fn bg1(acc: &CanvasModel) -> Option<&rf_enhance::stitcher::Canvas> {
    acc.scene_canvases(1).map(|s| &s[0])
}

/// BG1 cells known to both: `(compared, equal by palette index)`.
fn compare_bg1(a: &CanvasModel, b: &CanvasModel) -> (usize, usize) {
    let (Some(ca), Some(cb)) = (bg1(a), bg1(b)) else {
        return (0, 0);
    };
    let (ox, oy) = ca.origin();
    let (mut both, mut same) = (0, 0);
    for y in 0..ca.height() as i64 {
        for x in 0..ca.width() as i64 {
            if let (Some(pa), Some(pb)) = (ca.get(ox + x, oy + y), cb.get(ox + x, oy + y)) {
                both += 1;
                same += usize::from(pa.palette_index == pb.palette_index);
            }
        }
    }
    (both, same)
}

struct Run {
    scout: Scout,
    secs: f64,
}

fn run_scout(rom: &[u8], st: &EmuStepper, live: &CanvasModel, p: ScoutPlan) -> Run {
    let snap = st.snapshot().expect("snapshot");
    let t0 = std::time::Instant::now();
    let mut scout = Scout::start(rom, snap, p, live, SceneId(1), (W, H)).expect("scout");
    let scrolls = snes_layer_scrolls;
    // Bounded by the plan's own frame budget per pass (law 8); the cap here
    // is a backstop.
    for _ in 0..10_000 {
        if scout.step(&scrolls) != ScoutStatus::Running {
            break;
        }
    }
    Run {
        scout,
        secs: t0.elapsed().as_secs_f64(),
    }
}

fn width(acc: &CanvasModel) -> usize {
    bg1(acc).map_or(0, |c| c.width())
}

/// The recipes, from one snapshot of the live machine at the start of
/// Yoshi's Island 1, against a real walk through the same level.
#[test]
#[ignore = "needs the SMW ROM"]
fn scout_strategies() {
    let Some(path) = rom_path() else {
        println!("SKIP: no ROM");
        return;
    };
    let rom = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let (mut st, mut sink) = open().unwrap();
    reach_yi1(&mut st, &mut sink);
    let mut live = CanvasModel::new();
    let mut prev: Scrolls = snes_layer_scrolls(&st);
    // Frame 0 of the reference walk: the walk below replays the observed
    // run exactly (an enemy stands next to Mario at the start).
    live_frame(
        &mut st,
        &mut sink,
        &mut live,
        &mut prev,
        &[SnesButton::Right, SnesButton::B],
    );

    let cam = [0x7E_001Au32, 0x7E_1462];
    let tries: Vec<(&str, ScoutPlan)> = vec![
        (
            "a: write the camera",
            plan(ScoutStrategy::CameraWrite, &[], &cam, 8),
        ),
        (
            "b: move $D1 only",
            plan(ScoutStrategy::PlayerMove, &[0x7E_00D1], &cam[..1], 8),
        ),
        (
            "b: move $94",
            plan(ScoutStrategy::PlayerMove, &[0x7E_0094], &cam[..1], 8),
        ),
        (
            "b: the profile's recipe ($94, Mario held above the screen)",
            profile_plan(),
        ),
    ];
    let mut runs = Vec::new();
    for (label, p) in tries {
        let r = run_scout(&rom, &st, &live, p);
        println!(
            "[{label}] passes ended {:?}; {} frames in {:.1}s ({:.0} fps); BG1 canvas {} px wide",
            r.scout.ends,
            r.scout.frames_run(),
            r.secs,
            r.scout.frames_run() as f64 / r.secs,
            width(r.scout.model())
        );
        runs.push((label, r));
    }

    // The reference: a real walk right with hops, timed (the cost of a live
    // frame including stitching, on this machine right now).
    let t0 = std::time::Instant::now();
    let walk = 599u32;
    for f in 1..=walk {
        let keys: &[SnesButton] = if (f / 18) % 2 == 0 {
            &[SnesButton::Right, SnesButton::B]
        } else {
            &[SnesButton::Right]
        };
        live_frame(&mut st, &mut sink, &mut live, &mut prev, keys);
    }
    let live_ms = t0.elapsed().as_secs_f64() * 1000.0 / f64::from(walk);
    println!(
        "live walk: {walk} frames at {live_ms:.1} ms/frame, BG1 canvas {} px wide, camera {}",
        width(&live),
        word(&st, 0x1A)
    );
    if let Some(c) = bg1(&live) {
        dump_canvas(c, "live-walk");
    }
    for (label, r) in &runs {
        let (both, same) = compare_bg1(r.scout.model(), &live);
        let pct = if both > 0 {
            100.0 * same as f64 / both as f64
        } else {
            0.0
        };
        println!(
            "[{label}] vs the live walk: {both} BG1 cells known to both, {same} equal ({pct:.1}%); scout {:.1} ms/frame",
            r.secs * 1000.0 / r.scout.frames_run().max(1) as f64
        );
    }
    let (_, profile_run) = runs.last().unwrap();
    if let Some(c) = bg1(profile_run.scout.model()) {
        dump_canvas(c, "scout-profile");
    }

    // What the recipes did, as assertions (the evidence in the note).
    let w = |i: usize| width(runs[i].1.scout.model());
    assert!(
        w(0) < 300,
        "writing the camera is snapped back: {} px",
        w(0)
    );
    assert!(w(1) < 300, "$D1 is recopied from $94: {} px", w(1));
    assert!(
        w(3) > 4800,
        "the profile's recipe covers the level: {} px",
        w(3)
    );
    let (both, same) = compare_bg1(profile_run.scout.model(), &live);
    assert!(
        both > 30_000,
        "the walk and the scout overlap a lot: {both}"
    );
    assert_eq!(same, both, "every shared cell agrees");
}

/// False-colour dump of a canvas (palette index hashed to a colour; unknown
/// cells transparent) under `target/screens-probe/` for a quick look. The
/// real-colour picture is the GUI shot's job.
fn dump_canvas(c: &rf_enhance::stitcher::Canvas, label: &str) {
    let (w, h) = (c.width(), c.height());
    if w == 0 || h == 0 {
        return;
    }
    let (ox, oy) = c.origin();
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            if let Some(p) = c.get(ox + x as i64, oy + y as i64) {
                let v = u32::from(p.palette_index).wrapping_mul(2_654_435_761);
                rgba[i] = 60 + (v >> 8) as u8 / 2;
                rgba[i + 1] = 60 + (v >> 16) as u8 / 2;
                rgba[i + 2] = 60 + (v >> 24) as u8 / 2;
                rgba[i + 3] = 255;
            }
        }
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screens-probe");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{label}.png"));
    image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8).expect("png");
    println!("PNG {}", path.display());
}

/// A restored second machine draws the same frames as the live one for the
/// same input (the scout's whole premise).
#[test]
#[ignore = "needs the SMW ROM"]
fn restore_fidelity() {
    let Some(path) = rom_path() else {
        println!("SKIP: no ROM");
        return;
    };
    let rom = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let (mut st, mut sink) = open().unwrap();
    reach_yi1(&mut st, &mut sink);
    run(&mut st, &mut sink, &[SnesButton::Right, SnesButton::B], 1);
    let snap = st.snapshot().expect("snapshot");
    let mut st2 = EmuStepper::open(&rom).unwrap();
    st2.restore(&snap).unwrap();
    let mut sink2 = Sink::new();
    for f in 0..40 {
        let keys = [SnesButton::Right, SnesButton::B];
        st.latch_and_advance_frame(pad(&keys), &mut sink);
        st2.latch_and_advance_frame(pad(&keys), &mut sink2);
        let (a, b) = (
            sink.bundle.take(st.frame_count()),
            sink2.bundle.take(st2.frame_count()),
        );
        let diff = a.video.iter().zip(&b.video).filter(|(x, y)| x != y).count();
        assert_eq!(diff, 0, "frame {f}: {diff} pixels differ after a restore");
    }
}

/// Which work-RAM byte says "the player is in a level, playing"? Logs
/// run-lengths of a few candidates across boot, menus, the map, the level,
/// a death and what follows it.
#[test]
#[ignore = "needs the SMW ROM"]
fn observe_game_mode() {
    let Some((mut st, mut sink)) = open() else {
        return;
    };
    let mut last: Option<(u8, u8, u8)> = None;
    let mut since = 0u64;
    let mut frames = 0u64;
    let mut log = |st: &EmuStepper, frames: u64| {
        let now = (
            st.peek_bus(0x7E_0100),
            st.peek_bus(0x7E_0071),
            st.peek_bus(0x7E_009D),
        );
        if last != Some(now) {
            if let Some(l) = last {
                println!(
                    "  frames {since:5}..{frames:5}: $0100={:#04x} $71={:#04x} $9D={:#04x}",
                    l.0, l.1, l.2
                );
            }
            last = Some(now);
            since = frames;
        }
    };
    // The same script as `reach_yi1`, one frame at a time, then a long idle
    // (the first enemy walks into a standing Mario: a death and its restart).
    let script: Vec<(Vec<SnesButton>, u32)> = {
        let mut v: Vec<(Vec<SnesButton>, u32)> = vec![(vec![], 200)];
        for i in 0..10 {
            v.push((
                vec![if i % 2 == 0 {
                    SnesButton::Start
                } else {
                    SnesButton::A
                }],
                6,
            ));
            v.push((vec![], 108));
        }
        v.push((vec![], 300));
        v.push((vec![SnesButton::A], 8));
        v.push((vec![], 408));
        v.push((vec![SnesButton::Left], 8));
        v.push((vec![], 158));
        v.push((vec![SnesButton::A], 8));
        v.push((vec![], 1500));
        v
    };
    for (keys, n) in script {
        for _ in 0..n {
            run(&mut st, &mut sink, &keys, 1);
            frames += 1;
            log(&st, frames);
        }
    }
    println!("  (end) frames {since}..{frames}");
}

/// W32-08: every layer on the main screen gets a canvas whatever the
/// widescreen policy says (the status bar, BG3, is kept narrow by the default
/// `auto` policy yet is stitched here, and flagged HUD from its motion).
///
/// **Finding: the far hills are NOT on the main screen.** On Yoshi's Island 1
/// BG2 is never on TM (`layers on the main screen` below), the frame holds no
/// `Background(1)` pixel at all, and 72 % of its pixels are labelled
/// `Backdrop`: the hills reach the picture through colour math with the SUB
/// screen, and `PpuPixel::layer` names only the main-screen winner. So no
/// stitcher can see them in the bundle; the BG2 canvas stays empty here.
/// Prints what each layer holds and dumps BG1 under `target/screens-probe/`.
#[test]
#[ignore = "needs the SMW ROM"]
fn parallax_layers_get_canvases() {
    let Some(path) = rom_path() else {
        println!("SKIP: no ROM");
        return;
    };
    let rom = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let (mut st, mut sink) = open().unwrap();
    reach_yi1(&mut st, &mut sink);
    let views = st.bg_layer_views().expect("snes");
    let enabled: Vec<bool> = views.iter().map(|v| v.enabled).collect();
    println!("layers on the main screen: {enabled:?}");
    let mut live = CanvasModel::new();
    let mut prev: Scrolls = snes_layer_scrolls(&st);
    live_frame(
        &mut st,
        &mut sink,
        &mut live,
        &mut prev,
        &[SnesButton::Right, SnesButton::B],
    );
    let r = run_scout(&rom, &st, &live, profile_plan());
    let acc = r.scout.model();
    {
        // What the picture itself labels: which layer each pixel's winner is.
        let b = sink.bundle.take(st.frame_count());
        let mut hist = std::collections::BTreeMap::new();
        for p in &b.video {
            *hist.entry(format!("{:?}", p.layer)).or_insert(0usize) += 1;
        }
        println!("pixel layers on the level's first frame: {hist:?}");
    }
    let set = acc.scene_canvases(1).expect("scout canvases");
    for (n, c) in set.iter().enumerate() {
        println!(
            "BG{} scout canvas {}x{} px, {} known cells",
            n + 1,
            c.width(),
            c.height(),
            c.cells().iter().flatten().count()
        );
    }
    dump_canvas(&set[0], "w32_08_bg1");
    let hud = acc.hud_layers(1);
    println!("HUD layers by the scout's own motion evidence: {hud:?}");
    assert!(set[0].width() > 1000, "BG1 stitched across the level");
    assert!(
        set[2].width() > 0,
        "BG3 has a canvas although the policy keeps it narrow"
    );
    assert!(!hud[0] && !hud[1], "the level layers are not HUD: {hud:?}");
    assert!(hud[2], "the status bar is: {hud:?}");
}
