//! **W38-04: the widescreen finders on Super Mario World** (ROM-gated, law 5).
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
use rf_harness::object_tables::{snes_cam, snes_step_frame, Console, Recorder};
use rf_harness::widescreen::{
    observe_snes, rows_toml, snes_patch, snes_snaps, try_candidates, TrialSetup,
};
use rf_input::SnesButton;
use rf_rebuild::objtable::{infer as infer_tables, to_row};
use rf_rebuild::widescreen::{
    camera_aliases, find_camera, find_x_hi, hud_watch, infer, oam_shadow, slots_from_rows,
    static_sprites, widen_values, Candidate, Kind, View, Watch,
};
use rf_snes::core::SnesCore;

const FILE: &str = "Super Mario World (USA).zip";
const RAM_BASE: u32 = 0x7E_0000;
const TRIALS: usize = 8;

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
fn session() -> Vec<u16> {
    let mut pad = frames_of(TO_LEVEL);
    let start = pad.len();
    pad.extend(frames_of(PLAY));
    let more = frames_of("R+B14 R22");
    while pad.len() < start + 1500 {
        pad.extend(&more);
    }
    pad
}

fn inputs() -> Vec<InputFrame> {
    session()
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

fn baseline(raw: &[u8]) -> CoverageLog {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    for mask in session() {
        core.system_mut().bus.joypads.ports[0] = mask;
        core.run_frame(&InputFrame::empty(), &mut Null);
    }
    core.take_coverage().expect("armed")
}

#[test]
#[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR); many minutes in release"]
fn smw_widescreen_candidates_and_lockstep_trials() {
    let Some(raw) = rom_path().and_then(|p| load_rom(&p)) else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let t0 = Instant::now();
    let atlas = build_snes_atlas(&raw, &baseline(&raw)).expect("atlas");
    println!(
        "atlas: {} functions in {:?}",
        atlas.functions.len(),
        t0.elapsed()
    );
    let recipe = rf_profiles::load_recipe_file(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles/snes/super-mario-world/recipe.toml"),
    )
    .expect("recipe");

    // 1. The trace: object table, camera, HUD sprites.
    let ins = inputs();
    let lead = frames_of(TO_LEVEL).len() - 60;
    let mut core = SnesCore::load(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    let mut rec = Recorder::new(Console::Snes, false);
    for (f, input) in ins.iter().enumerate() {
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
    if let Some(off) = find_x_hi(&table, &cam, &trace) {
        table.x_hi = Some(off);
    }
    println!("x page column offset {:?}", table.x_hi);
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
    let facts = observe_snes(&raw, &atlas, &ins, lead, watch).expect("observed run");
    let cands = infer(&facts);
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
    let plain = snes_snaps(&raw, &ins, |_, _| {}).expect("plain run");
    let view = View {
        ram_base: RAM_BASE,
        table: table.clone(),
        cam_lo: cam.lo,
        cam_hi: cam.hi,
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
            run: &|image| snes_snaps(image, &ins, |_, _| {}),
            values: 1,
        },
        &chosen,
        &mut |l| println!("{l}"),
    )
    .expect("trials");
    println!("{} candidates tried in {:?}", tried.len(), t.elapsed());
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
    let toml = rows_toml(
        &atlas,
        &recipe,
        &tried,
        "widescreen-objects",
        "widescreen-candidates",
        &[],
        "SMW Yoshi's Island 1 play",
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
