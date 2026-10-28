//! **W38-04b: the widescreen finders on Super Mario World over a long stretch** (ROM-gated, law 5).
//!
//! W38-04 watched one level (1,593 frames, 10 state flips) and kept nothing.
//! This session is power-on, two TAS movies (W37-00 import, resets applied) and
//! the Yoshi's Island 1 script back to back, each segment after a hard reset:
//! many levels, many sprites spawning and despawning. The headline number is
//! the state-flip count. Per-candidate first-divergence reasons are printed.
//!
//! The SNES half of `w38_04_widescreen_smb`: a plain run records the W38-02
//! trace (object table, camera, HUD sprites found without naming an address),
//! an observed run lists the immediate-op sites that touch object-x and camera
//! bytes with a confidence, and the best candidates are tried with wider
//! constants on copies of the image, a plain run against the unmodified plain
//! run. Only offsets, counts and verdicts are printed (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_04_widescreen_smw -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;
use std::time::Instant;

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_snes_atlas;
use rf_harness::movie::{self, Movie};
use rf_harness::movie_replay::plan;
use rf_harness::object_tables::{snes_cam, snes_step_frame, Console, Recorder};
use rf_harness::widescreen::{
    observe_snes_hooked, rows_toml, snes_patch, snes_snaps, try_candidates, try_windows, TrialSetup,
};
use rf_input::SnesButton;
use rf_rebuild::objtable::{infer as infer_tables, to_row};
use rf_rebuild::widescreen::{
    camera_aliases, find_camera, find_x_page, hud_watch, infer, infer_all, oam_shadow,
    slots_from_rows, static_sprites, widen_values, window_pairs, Candidate, Kind, Snap, View,
    Watch, WideFacts, XPage,
};
use rf_snes::core::SnesCore;

const FILE: &str = "Super Mario World (USA).zip";
const RAM_BASE: u32 = 0x7E_0000;
const TRIALS: usize = 16;
/// Repetitions of one 36-frame stride in a patrol leg.
const PATROL_LEG: usize = 12;

/// Menus to Yoshi's Island 1 and play (the W38-01 script, plain frames).
const TO_LEVEL: &str = "w200 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 w300 A8 w408 A8 w200 R40 w20 L40 w20 R40 w20 L40 w20 R40 w20 L40 w20 R60 w20 R200 w300 L8 w158 A8 w158";
const PLAY: &str = "L2 w3 B8 w52 B8 w52 B8 w52 B8 w52 R+B14 R22 R+B14 R22 S6 w100 S6 w60 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22";

fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("snes").join(FILE);
    p.exists().then_some(p)
}

fn load_rom(path: &std::path::Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).ok()?;
        if !entry.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).ok()?;
        if matches!(
            rf_cart::Cartridge::load(&buf),
            Ok(rf_cart::Cartridge::Snes { .. })
        ) {
            return Some(buf);
        }
    }
    None
}

fn frames_of(text: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let (mask, n) = if let Some(n) = tok.strip_prefix('w') {
            (0u16, n.parse::<usize>().expect("wN"))
        } else {
            let split = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
            let (b, n) = tok.split_at(split);
            let mask = b.split('+').fold(0u16, |m, c| {
                let bit = match c {
                    "A" => SnesButton::A,
                    "B" => SnesButton::B,
                    "X" => SnesButton::X,
                    "Y" => SnesButton::Y,
                    "S" => SnesButton::Start,
                    "E" => SnesButton::Select,
                    "U" => SnesButton::Up,
                    "D" => SnesButton::Down,
                    "L" => SnesButton::Left,
                    "R" => SnesButton::Right,
                    other => panic!("button {other}"),
                }
                .bit();
                m | (1 << bit)
            });
            (mask, n.parse().expect("N"))
        };
        out.extend(std::iter::repeat_n(mask, n));
    }
    out
}

/// Menus to the level, then play until the level has seen 1,500 frames.
fn script_base() -> Vec<u16> {
    let mut pad = frames_of(TO_LEVEL);
    let start = pad.len();
    pad.extend(frames_of(PLAY));
    let more = frames_of("R+B14 R22");
    while pad.len() < start + 1500 {
        pad.extend(&more);
    }
    pad
}

/// The base script plus a patrol.
fn script() -> Vec<u16> {
    let mut pad = script_base();
    // The patrol: right then back left over the level, so sprites are loaded
    // on the way out and again (and dropped) on the way back; `A` now and then
    // re-enters a level after a death.
    let right = frames_of(&"R+B14 R22 ".repeat(PATROL_LEG));
    let left = frames_of(&"L+B14 L22 ".repeat(PATROL_LEG));
    let enter = frames_of("w30 A6 w120");
    for _ in 0..env_usize("PATROLS", 12) {
        pad.extend(&right);
        pad.extend(&left);
        pad.extend(&enter);
    }
    pad
}

fn tas_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("RF_SCREENS_ROMS_DIR")
            .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string()),
    )
    .join("tas")
}

fn movie_file(var: &str, default: &str) -> Option<PathBuf> {
    let p = std::env::var(var).map_or_else(|_| tas_dir().join(default), PathBuf::from);
    p.exists().then_some(p)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// One power-on run: its pads and any resets the movie itself makes.
struct Seg {
    name: &'static str,
    ins: Vec<InputFrame>,
    resets: Vec<Option<ResetKind>>,
}

/// The long session (module doc): each segment is played on a fresh core from
/// power-on (`SnesCore::reset` only restarts the CPU, so a mid-run "hard
/// reset" is not a power-on and the menu script and the movies drift).
fn session() -> Vec<Seg> {
    let mut segs = Vec::new();
    // (name, movie, idle frames before it, frames to run): the offsets are
    // W37-00's sweep winners (docs/research/W37-00-movie-import.md).
    let mut sources: Vec<(&'static str, Movie, usize, usize)> = Vec::new();
    if let Some(p) = movie_file("RF_W37_SMW_LSMV", "4144/x/brunovisnadi-smw-warps.lsmv") {
        let m = movie::lsmv::parse_archive(&std::fs::read(p).expect("reads")).expect("lsmv");
        sources.push(("warps", m, 3, env_usize("CAP_WARPS", 8_000)));
    }
    if let Some(p) = movie_file(
        "RF_W37_SMW_SMV",
        "2307/x/bahamete,kaizoman666,masterjun-supermarioworld-96exit.smv",
    ) {
        let m = movie::smv::parse(&std::fs::read(p).expect("reads")).expect("smv");
        sources.push(("96exit", m, 4, env_usize("CAP_96", 5_000)));
    }
    for (name, m, delay, cap) in sources {
        let pl = plan(&[], delay, 0, &m);
        let mut seg = Seg {
            name,
            ins: Vec::new(),
            resets: Vec::new(),
        };
        for f in pl.frames.iter().take(cap) {
            let mut inp = InputFrame::empty();
            inp.ports[0] = f.pads[0];
            inp.ports[1] = f.pads[1];
            seg.ins.push(inp);
            seg.resets.push(f.reset.map(|r| match r {
                movie::Reset::Soft => ResetKind::Soft,
                movie::Reset::Hard => ResetKind::Hard,
            }));
        }
        segs.push(seg);
    }
    let pads = script();
    segs.push(Seg {
        name: "yi1-script+patrol",
        resets: vec![None; pads.len()],
        ins: pads
            .into_iter()
            .map(|m| {
                let mut f = InputFrame::empty();
                f.ports[0] = m;
                f
            })
            .collect(),
    });
    segs.retain(|s| !s.ins.is_empty());
    segs
}

/// The Yoshi's Island 1 script alone, as W38-04 played it (the trace for the table and camera).
fn inputs() -> Vec<InputFrame> {
    script_base()
        .into_iter()
        .map(|m| {
            let mut f = InputFrame::empty();
            f.ports[0] = m;
            f
        })
        .collect()
}

struct Null;
impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn baseline(raw: &[u8], segs: &[Seg]) -> CoverageLog {
    let mut all: Option<CoverageLog> = None;
    for seg in segs {
        let mut core = SnesCore::load(raw).expect("loads");
        core.reset(ResetKind::Hard);
        core.set_coverage_recording(true);
        for (inp, reset) in seg.ins.iter().zip(&seg.resets) {
            if let Some(r) = reset {
                core.reset(*r);
            }
            core.system_mut().bus.joypads.ports[0] = inp.ports[0];
            core.run_frame(&InputFrame::empty(), &mut Null);
        }
        let log = core.take_coverage().expect("armed");
        match all.as_mut() {
            None => all = Some(log),
            Some(a) => {
                assert!(a.merge(&log), "coverage logs of one cartridge merge");
            }
        }
    }
    all.expect("a segment")
}

/// A plain run of every segment, snaps concatenated (law of `compare_trial`:
/// frame-by-frame against the same layout).
fn snaps_of(image: &[u8], segs: &[Seg]) -> Result<Vec<Snap>, String> {
    let mut out = Vec::new();
    for seg in segs {
        out.extend(snes_snaps(image, &seg.ins, |f, core| {
            if let Some(r) = seg.resets[f] {
                core.reset(r);
            }
        })?);
    }
    Ok(out)
}

#[test]
#[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR); many minutes in release"]
fn smw_long_widescreen_candidates_and_lockstep_trials() {
    let Some(raw) = rom_path().and_then(|p| load_rom(&p)) else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let recipe = rf_profiles::load_recipe_file(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles/snes/super-mario-world/recipe.toml"),
    )
    .expect("recipe");

    // 1. The trace (Yoshi's Island 1, as W38-04): object table, camera, HUD
    // sprites. The long session below is what is observed and trialed.
    let ins1 = inputs();
    let lead = frames_of(TO_LEVEL).len() - 60;
    let segs = session();
    println!(
        "long session: {} frames in {} power-on segments {:?}",
        segs.iter().map(|s| s.ins.len()).sum::<usize>(),
        segs.len(),
        segs.iter()
            .map(|s| (s.name, s.ins.len()))
            .collect::<Vec<_>>()
    );
    let mut core = SnesCore::load(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    let mut rec = Recorder::new(Console::Snes, false);
    for (f, input) in ins1.iter().enumerate() {
        if f >= lead {
            let cap = snes_step_frame(&mut core, input);
            rec.push(cap, snes_cam(&core));
        } else {
            core.system_mut().bus.joypads.ports[0] = input.ports[0];
            core.run_frame(input, &mut Null);
        }
    }
    let trace = rec.trace;
    let found = infer_tables(&trace);
    let table = to_row(&found[0], rf_rebuild::Grade::Candidate, "; SMW YI1");
    let table = slots_from_rows(&table, &recipe.ram);
    println!(
        "table base {:#x} slots {} stride {} x_lo {:?} x_hi {:?} state {:?}",
        table.base, table.slots, table.stride, table.x_lo, table.x_hi, table.state
    );
    let cam = find_camera(&trace).expect("a camera");
    // The finder binds x low bytes to sprites; the page column is found against
    // the camera.
    let mut table = table;
    let page = find_x_page(&table, &cam, &trace);
    let mut x_page_zero = false;
    match page {
        Some(XPage::Column(off)) => table.x_hi = Some(off),
        Some(XPage::Zero) => x_page_zero = true,
        None => {}
    }
    println!("x page {page:?}");
    if let Some(XPage::Column(o)) = page {
        let tb = table.base as usize - 0x7E_0000;
        let agree = trace
            .frames
            .iter()
            .filter(|f| (0..8).all(|i| f.ram[tb + o as usize + i] == f.ram[0x14E0 + i]))
            .count();
        let nz = trace
            .frames
            .iter()
            .filter(|f| (0..8).any(|i| f.ram[tb + o as usize + i] != 0))
            .count();
        let real_nz = trace
            .frames
            .iter()
            .filter(|f| (0..8).any(|i| f.ram[0x14E0 + i] != 0))
            .count();
        println!("dbg column agrees with 0x14e0 in {agree}/{} frames; column nonzero {nz}, 0x14e0 nonzero {real_nz}", trace.frames.len());
    }
    if std::env::var("PAGE_ONLY").is_ok() {
        return;
    }
    println!(
        "camera lo {:#x} hi {:?} (recipe camera_x row is 0x7e001a)",
        RAM_BASE + cam.lo as u32,
        cam.hi.map(|h| RAM_BASE + h as u32)
    );
    let statics = static_sprites(&trace);
    let shadow = oam_shadow(&trace);
    println!(
        "static sprites {:?}; oam shadow {:?}",
        statics
            .iter()
            .map(|s| (s.index, s.x, s.y))
            .collect::<Vec<_>>(),
        shadow
    );
    let t0 = Instant::now();
    let atlas = build_snes_atlas(&raw, &baseline(&raw, &segs)).expect("atlas");
    println!(
        "atlas: {} functions in {:?}",
        atlas.functions.len(),
        t0.elapsed()
    );
    let mut watch = Watch::default();
    for i in 0..table.slots {
        for off in [table.x_lo, table.x_hi].into_iter().flatten() {
            watch.obj_x.insert(table.base + off + i * table.stride);
        }
        if let Some(off) = table.state {
            watch.obj_state.insert(table.base + off + i * table.stride);
        }
    }
    watch.cam.insert(RAM_BASE + cam.lo as u32);
    watch.cam.extend(cam.hi.map(|h| RAM_BASE + h as u32));
    let aliases = camera_aliases(&trace, &cam);
    println!("camera aliases {} bytes", aliases.len());
    watch
        .cam
        .extend(aliases.iter().map(|&a| RAM_BASE + a as u32));
    if let Some(sh) = shadow {
        watch.hud = hud_watch(&trace, sh, &statics);
    }

    // 2. The observed run.
    let t = Instant::now();
    let mut facts = WideFacts::default();
    for seg in &segs {
        let f = observe_snes_hooked(&raw, &atlas, &seg.ins, 0, watch.clone(), |f, core| {
            if let Some(r) = seg.resets[f] {
                core.reset(r);
            }
        })
        .expect("observed run");
        println!(
            "segment {}: {} frames, {} flips up / {} down",
            seg.name,
            seg.ins.len(),
            f.flips_up,
            f.flips_down
        );
        facts.merge(&f);
    }
    let cands = infer(&facts);
    let all_cands = infer_all(&facts);
    println!(
        "observed {} frames in {:?}: {} immediate-op sites, {} flips up / {} down, {} cam writes, {} candidates",
        facts.frames,
        t.elapsed(),
        facts.sites.len(),
        facts.flips_up,
        facts.flips_down,
        facts.cam_writes,
        cands.len()
    );
    for c in &cands {
        println!(
            "CAND {:<16} rom {:#07x} fn {:?} {:?} w{} imm {:#06x} score {:>3} {}  [{}]",
            c.kind.name(),
            c.site,
            c.function,
            c.op.map(|o| o.name()),
            c.width,
            c.imm,
            c.score,
            c.band.name(),
            c.evidence
        );
    }
    assert!(!cands.is_empty(), "candidates are found");

    // 3. Trials.
    let plain = snaps_of(&raw, &segs).expect("plain run");
    // Which levels the plain run was in (mode 0x14 at $7E0100, translevel at
    // $7E13BF): how much play the headline flip count stands on.
    let mut levels = std::collections::BTreeSet::new();
    let mut in_level_frames = 0usize;
    let mut entries = 0usize;
    let mut was_in = false;
    let mut live_peak = 0usize;
    for s in &plain {
        let inl = s.ram[0x100] == 0x14;
        if inl {
            in_level_frames += 1;
            levels.insert(s.ram[0x13BF]);
            if !was_in {
                entries += 1;
            }
            let live = (0..table.slots)
                .filter(|&i| {
                    table.state.is_some_and(|o| {
                        s.ram[(table.base - RAM_BASE + o + i * table.stride) as usize] != 0
                    })
                })
                .count();
            live_peak = live_peak.max(live);
        }
        was_in = inl;
    }
    println!(
        "plain run: {} frames, {in_level_frames} in a level, {entries} level entries, {} distinct translevels {:x?}, peak live slots {live_peak}",
        plain.len(),
        levels.len(),
        levels
    );
    let view = View {
        ram_base: RAM_BASE,
        table: table.clone(),
        cam_lo: cam.lo,
        cam_hi: cam.hi,
        x_page_zero,
    };
    let trials: usize = std::env::var("TRIALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(TRIALS);
    let chosen: Vec<&Candidate> = cands
        .iter()
        .filter(|c| c.kind != Kind::ScreenConst && !widen_values(c).is_empty())
        .take(trials)
        .collect();
    let t = Instant::now();
    let tried = try_candidates(
        &TrialSetup {
            raw: &raw,
            view: &view,
            plain: &plain,
            patch: &|r, at, b| snes_patch(r, at, b),
            run: &|image| snaps_of(image, &segs),
            values: 2,
        },
        &chosen,
        &mut |l| println!("{l}"),
    )
    .expect("trials");
    println!("{} candidates tried in {:?}", tried.len(), t.elapsed());
    // The sprite on-screen windows: `adc #lo` / `cmp #hi` pairs widened
    // together (each alone moves only one edge).
    let mut tried = tried;
    let pairs = window_pairs(&all_cands);
    println!(
        "{} adc/cmp window pairs among {} candidates",
        pairs.len(),
        all_cands.len()
    );
    for p in &pairs {
        println!(
            "WINDOW adc rom {:#07x} {:#06x} + cmp rom {:#07x} {:#06x}",
            p.adc.operand, p.adc.imm, p.cmp.operand, p.cmp.imm
        );
    }
    let t = Instant::now();
    let widths = [0x20u16, 0x40, 0x80];
    let windows = try_windows(
        &TrialSetup {
            raw: &raw,
            view: &view,
            plain: &plain,
            patch: &|r, at, b| snes_patch(r, at, b),
            run: &|image| snaps_of(image, &segs),
            values: 1,
        },
        &pairs,
        &widths,
        &mut |l| println!("{l}"),
    )
    .expect("window trials");
    println!("{} window pairs tried in {:?}", pairs.len(), t.elapsed());
    for w in windows {
        match tried.iter().position(|t| t.cand.operand == w.cand.operand) {
            Some(i) if w.best.is_some() => tried[i] = w,
            Some(_) => {}
            None => tried.push(w),
        }
    }
    for t in &tried {
        println!(
            "RESULT {:<16} (reads said {:<16}) rom {:#07x} grade {} value {:?} partners {:x?}",
            t.kind().name(),
            t.cand.kind.name(),
            t.cand.operand,
            t.grade().as_str(),
            t.best.as_ref().map(|b| b.0),
            t.partners
        );
    }
    for t in tried
        .iter()
        .filter(|t| t.grade() != rf_rebuild::Grade::Confirmed)
    {
        for (v, tr) in &t.alone {
            let d = tr.first.as_ref();
            println!(
                "NOTKEPT rom {:#07x} {:<16} {:#06x}->{:#06x}: {} (first divergence frame {:?}, off {} on {} other {} oam {} cam {}; reach R{} L{})",
                t.cand.operand,
                t.cand.kind.name(),
                t.cand.imm,
                v,
                tr.why_not(),
                d.map(|d| d.frame),
                d.map_or(0, |d| d.off_screen.len()),
                d.map_or(0, |d| d.on_screen.len()),
                d.map_or(0, |d| d.bookkeeping.len()),
                d.is_some_and(|d| d.oam),
                d.is_some_and(|d| d.camera),
                tr.gain_right,
                tr.gain_left
            );
        }
    }
    let toml = rows_toml(
        &atlas,
        &recipe,
        &tried,
        "widescreen-objects",
        "widescreen-candidates",
        &[],
        "SMW long session: two TAS movies and Yoshi's Island 1",
    );
    if let Ok(out) = std::env::var("W3804_ROWS_OUT") {
        std::fs::write(out, &toml).expect("writes the rows");
    }
    println!(
        "KEEPERS {:x?}",
        tried
            .iter()
            .filter(|t| t.grade() == rf_rebuild::Grade::Confirmed)
            .map(|t| t.cand.operand)
            .collect::<Vec<_>>()
    );
}
