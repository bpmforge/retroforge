//! **W33-06: the scout's self-check and strategy ladder, on the three games
//! whose profiles have a `[scout]` table.**
//!
//! ROM-gated (law 5): every test is `#[ignore]` and skips with a message when
//! the ROM is absent. Each one drives the real [`ScoutDriver`] (the worker
//! thread, the self-check, the ladder, the merge) over a live canvas the test
//! builds from a short idle stretch at the start of a level, then reads the
//! telemetry. Run ONE at a time, from a cold canvas cache:
//!
//! ```text
//! cargo test --release -p retroforge --test w33_06_ladder -- --ignored --nocapture <name>
//! ```
//!
//! * `smb3_forced_step_8` -- the acceptance: 8 px per frame is refused and the
//!   scout finishes at a smaller step.
//! * `smb3_profile`, `mm2_profile`, `smw_profile` -- the profiles' own recipes
//!   still finish every pass.
//! * `smb3_probe_overlap` -- the measurement behind the design (where a
//!   too-fast scout disagrees with the live canvas).

use std::path::{Path, PathBuf};

use retroforge::scout::{
    LiveView, ScoutDir, ScoutDriver, ScoutPlan, ScoutSetup, ScoutStatus, ScoutStrategy,
    ScoutTelemetry,
};
use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreSink, FrameBundleBuilder, InputFrame, PpuPixel};
use rf_enhance::canvas::{scout_agreement, scout_agreement_aligned, CanvasModel, FrameFeed};
use rf_enhance::scene_identity::SceneId;

#[allow(dead_code, unused_imports)]
#[path = "w32_07_scout_smw.rs"]
mod smw;

use std::sync::atomic::Ordering::Relaxed;

const W: u16 = 256;
const H: u16 = 240;

fn nes_rom(file: &str) -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join("nes").join(file);
    if p.exists() {
        Some(p)
    } else {
        println!("SKIP: {file} not found");
        None
    }
}

struct Sink {
    bundle: FrameBundleBuilder,
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

fn pad(mask: u16) -> InputFrame {
    let mut f = InputFrame::empty();
    f.ports[0] = mask;
    f
}

/// NES port 0 bits (nesdev "Controller reading").
fn mask_of(tok: &str) -> u16 {
    tok.split('+').fold(0, |m, c| {
        m | match c {
            "A" => 1,
            "B" => 1 << 1,
            "E" => 1 << 2,
            "S" => 1 << 3,
            "U" => 1 << 4,
            "D" => 1 << 5,
            "L" => 1 << 6,
            "R" => 1 << 7,
            _ => 0,
        }
    })
}

/// `wN` idle N frames, `<buttons>N` hold N frames; every frame feeds `live`.
fn script(st: &mut EmuStepper, sink: &mut Sink, live: &mut CanvasModel, text: &str) {
    for tok in text.split_whitespace() {
        let (mask, n) = if let Some(n) = tok.strip_prefix('w') {
            (0, n.parse::<u32>().expect("wN"))
        } else {
            let split = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
            let (b, n) = tok.split_at(split);
            (mask_of(b), n.parse().expect("N"))
        };
        for _ in 0..n {
            st.latch_and_advance_frame(pad(mask), sink);
            let b = sink.bundle.take(st.frame_count());
            live.observe_frame(&b, &FrameFeed::bands());
        }
    }
}

const SMB3: &str = "Super Mario Bros. 3 (USA).zip";
const SMB3_TO_LEVEL: &str = "w300 S6 w120 S6 w400 R8 w60 U8 w60 A8 w300";
const MM2: &str = "Mega Man 2 (USA).zip";
const MM2_TO_AIRMAN: &str = "w400 S6 w120 S6 w300 S6 w300 U8 w20 S6 w800";

fn profile_plan(rel: &str) -> ScoutPlan {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let outcome = rf_profiles::load_file(&path).expect("the profile loads");
    ScoutPlan::from_profile(outcome.profile.scout.as_ref().expect("[scout]"))
}

/// A NES machine at the start of a level, with a live canvas of `idle` frames
/// standing still there.
fn nes_at_level(
    file: &str,
    to_level: &str,
    idle: u32,
) -> Option<(Vec<u8>, EmuStepper, CanvasModel, SceneId)> {
    let path = nes_rom(file)?;
    let rom = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let mut st = EmuStepper::open(&rom).expect("opens");
    st.set_event_mask(rf_core_api::EventMask::ALL);
    let mut sink = Sink {
        bundle: FrameBundleBuilder::new(W, H),
    };
    // The menus and the fade-in go to a throwaway model; the level's live
    // canvas starts when the level is on screen.
    script(&mut st, &mut sink, &mut CanvasModel::new(), to_level);
    let mut live = CanvasModel::new();
    script(&mut st, &mut sink, &mut live, &format!("w{idle}"));
    let scene = live.current_scene().expect("a scene");
    if let Some(r) = live.scout_reference(scene.0) {
        // A fresh model stitches the FIRST frame it sees of a scene (nothing
        // to call a zero-delta HUD band yet): the app's long-lived model, which
        // saw the level's first frame before the scene settled, holds nothing
        // at a standstill (reference cells 0 in the shots).
        println!(
            "live canvas after {idle} idle frames: {:?}",
            r.layers
                .iter()
                .map(|c| (c.width(), c.height()))
                .collect::<Vec<_>>()
        );
    }
    Some((rom, st, live, scene))
}

/// What a driven scout did.
struct Driven {
    telemetry: std::sync::Arc<ScoutTelemetry>,
}

/// Tick the driver (the live machine stands still) until the scout finished
/// or was given up, at most `secs`.
#[allow(clippy::too_many_arguments)] // a test driver: every part of the scene is named
fn drive(
    rom: &[u8],
    st: &EmuStepper,
    live: &mut CanvasModel,
    scene: SceneId,
    plan: ScoutPlan,
    scrolls: retroforge::scout::ScrollReader,
    size: (u16, u16),
    secs: u64,
) -> Driven {
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
            stepper: st,
            rom,
            canvases: live,
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
    println!(
        "scout: finished {} gave up {} aborted {}; {} frames, {} cells merged; agreement {:?}\u{2030} over {} shared cells; \
         deltas merged {} slow {} rejected {} unjudged {}; probes {} (rejected {}); passes ended {:?}\n  {}",
        telemetry.finished.load(Relaxed),
        telemetry.gave_up.load(Relaxed),
        telemetry.aborted.load(Relaxed),
        telemetry.frames.load(Relaxed),
        telemetry.cells.load(Relaxed),
        telemetry.agreement_permille(),
        telemetry.shared_cells.load(Relaxed),
        telemetry.deltas_merged.load(Relaxed),
        telemetry.deltas_slow.load(Relaxed),
        telemetry.deltas_rejected.load(Relaxed),
        telemetry.deltas_unjudged.load(Relaxed),
        telemetry.probes.load(Relaxed),
        telemetry.probes_rejected.load(Relaxed),
        telemetry.last_ends.lock().map(|e| e.clone()).unwrap_or_default(),
        telemetry.last_summary().unwrap_or_default(),
    );
    Driven { telemetry }
}

fn no_scrolls(_: &EmuStepper) -> Option<[Option<rf_enhance::canvas::LayerScroll>; 4]> {
    None
}

/// A finished scout whose every pass ended, with cells merged and agreement
/// reported.
fn assert_finished_both_passes(d: &Driven, passes: usize) {
    let t = &d.telemetry;
    assert_eq!(
        t.gave_up.load(Relaxed),
        0,
        "the check gave up on a recipe that works"
    );
    assert_eq!(t.finished.load(Relaxed), 1, "the scout finished");
    let ends = t.last_ends.lock().unwrap().clone();
    assert_eq!(ends.len(), passes, "every pass ended: {ends:?}");
    assert!(t.cells.load(Relaxed) > 0, "and merged cells");
    assert!(t.agreement_permille().is_some(), "agreement is reported");
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_forced_step_8() {
    let Some((rom, st, mut live, scene)) = nes_at_level(SMB3, SMB3_TO_LEVEL, 90) else {
        return;
    };
    let mut plan = profile_plan("../../profiles/nes/super-mario-bros-3/profile.toml");
    plan.settle_frames = 0;
    plan.step_px = 8;
    let d = drive(&rom, &st, &mut live, scene, plan, no_scrolls, (W, H), 600);
    let t = &d.telemetry;
    assert_finished_both_passes(&d, 2);
    let report = t.last_report.lock().unwrap().clone().expect("a report");
    let first = report.attempts.first().expect("a check");
    // The 8 px step is refused before one cell merged...
    assert_eq!(
        (first.what, first.step_px),
        ("probe", 8),
        "{}",
        report.summary()
    );
    assert_eq!(
        first.check.verdict,
        Some(rf_enhance::canvas::ScoutVerdict::Reject),
        "{}",
        report.summary()
    );
    // ...and the pass is retried at 4 and accepted.
    assert_eq!(
        report.winner(),
        Some((ScoutStrategy::PlayerMove, 4)),
        "{}",
        report.summary()
    );
    assert!(t.probes_rejected.load(Relaxed) >= 1);
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_profile() {
    let Some((rom, st, mut live, scene)) = nes_at_level(SMB3, SMB3_TO_LEVEL, 90) else {
        return;
    };
    let mut plan = profile_plan("../../profiles/nes/super-mario-bros-3/profile.toml");
    plan.settle_frames = 0;
    let d = drive(&rom, &st, &mut live, scene, plan, no_scrolls, (W, H), 600);
    assert_finished_both_passes(&d, 2);
    let report = d.telemetry.last_report.lock().unwrap().clone().unwrap();
    assert_eq!(
        report.winner(),
        Some((ScoutStrategy::PlayerMove, 4)),
        "{}",
        report.summary()
    );
}

#[test]
#[ignore = "needs Mega Man 2"]
fn mm2_profile() {
    let Some((rom, st, mut live, scene)) = nes_at_level(MM2, MM2_TO_AIRMAN, 90) else {
        return;
    };
    let mut plan = profile_plan("../../profiles/nes/mega-man-2/profile.toml");
    plan.settle_frames = 0;
    let d = drive(&rom, &st, &mut live, scene, plan, no_scrolls, (W, H), 600);
    assert_finished_both_passes(&d, 1);
    let report = d.telemetry.last_report.lock().unwrap().clone().unwrap();
    assert_eq!(
        report.winner().map(|w| w.0),
        Some(ScoutStrategy::Walk),
        "{}",
        report.summary()
    );
}

#[test]
#[ignore = "needs Super Mario World"]
fn smw_profile() {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let path = Path::new(&root)
        .join("snes")
        .join("Super Mario World (USA).zip");
    if !path.exists() {
        println!("SKIP: Super Mario World (USA) not found");
        return;
    }
    let rom = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let mut st = EmuStepper::open(&rom).expect("opens");
    let mut sink = smw::Sink::new();
    smw::reach_yi1(&mut st, &mut sink);
    // 60 live frames standing at the start of Yoshi's Island 1, stitched the
    // way the core thread does (per layer, at the scroll read after the
    // PREVIOUS frame).
    let mut live = CanvasModel::new();
    let mut prev = retroforge::core_thread::snes_layer_scrolls(&st);
    for _ in 0..60 {
        st.latch_and_advance_frame(InputFrame::empty(), &mut sink);
        let bundle = sink.bundle.take(st.frame_count());
        let now = retroforge::core_thread::snes_layer_scrolls(&st);
        let drawn_with = std::mem::replace(&mut prev, now);
        live.observe_frame_in(
            &bundle,
            SceneId(1),
            &FrameFeed::registers(drawn_with.as_ref()),
        );
    }
    let mut plan = profile_plan("../../profiles/snes/super-mario-world/profile.toml");
    plan.settle_frames = 0;
    let d = drive(
        &rom,
        &st,
        &mut live,
        SceneId(1),
        plan,
        retroforge::core_thread::snes_layer_scrolls,
        (256, 224),
        900,
    );
    assert_finished_both_passes(&d, 2);
}

/// Where does a too-fast scout disagree with what the live game has seen?
/// (The measurement behind the probe: the live canvas holds the screen the
/// player stands on, and the damage of 8 px per frame lies ahead of it.)
#[test]
#[ignore = "needs SMB3"]
fn smb3_probe_overlap() {
    let Some((rom, st, live, scene)) = nes_at_level(SMB3, SMB3_TO_LEVEL, 20) else {
        return;
    };
    let reference = live.scout_reference(scene.0).expect("live canvas");
    let lc = &reference.layers[0];
    for step in [4u16, 8] {
        let mut plan = profile_plan("../../profiles/nes/super-mario-bros-3/profile.toml");
        plan.step_px = step;
        plan.dirs = vec![ScoutDir::Right];
        plan.settle_frames = 0;
        let snap = st.snapshot().expect("snapshot");
        let mut scout =
            retroforge::scout::Scout::start(&rom, snap, plan, &live, scene, (W, H)).expect("scout");
        let mut n = 0u32;
        // Bounded: the pass's own frame budget.
        for _ in 0..4000u32 {
            let status = scout.step(&no_scrolls);
            n += 1;
            if n.is_multiple_of(30) || status != ScoutStatus::Running {
                if let Some(d) = scout.take_delta() {
                    let (a, s) = scout_agreement(lc, &d[0], None);
                    let al = scout_agreement_aligned(lc, &d[0], None);
                    println!(
                        "step {step} frame {n}: unaligned {a}/{s}, aligned {}/{} at dy {}",
                        al.agreed, al.shared, al.dy
                    );
                }
            }
            if status != ScoutStatus::Running {
                break;
            }
        }
    }
}
