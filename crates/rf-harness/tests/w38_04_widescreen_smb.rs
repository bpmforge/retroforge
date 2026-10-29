//! **W38-04: the widescreen finders on Super Mario Bros.** (ROM-gated, law 5).
//!
//! 1. A plain run records the W38-02 trace; the object table and the camera
//!    (`find_camera`) and the HUD sprites come from it, with no address named.
//! 2. An observed run of the same play lists the immediate-op sites that touch
//!    object-x and camera bytes (`rf_rebuild::widescreen`), with a confidence.
//!    **Ground truth:** the three bytes the W36-06 `live-enemies` mod edits must
//!    be among them.
//! 3. Each of the best candidates is tried with wider constants on a copy of the
//!    image, a plain run against the unmodified plain run; keepers change only
//!    off-screen state first (`compare_trial`).
//!
//! Only offsets, counts and verdicts are printed (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_04_widescreen_smb -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;
use std::time::Instant;

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::object_tables::{nes_cam, nes_prepare, nes_step_frame, Console, Recorder};
use rf_harness::widescreen::{
    hud_rows_toml, nes_patch, nes_snaps, observe_nes, rows_toml, try_candidates, try_hud,
    TrialSetup,
};
use rf_nes::core::NesCore;
use rf_nes::CpuBus as _;
use rf_rebuild::objtable::{infer as infer_tables, to_row};
use rf_rebuild::widescreen::{
    camera_aliases, find_camera, hud_watch, infer, oam_shadow, slots_from_rows, static_sprites,
    widen_values, Candidate, Kind, View, Watch,
};
use rf_rebuild::{resolve_patches, Atlas};

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u8 = 1 << 3;
const A: u8 = 1 << 0;
const RIGHT: u8 = 1 << 7;
const LEAD_IN: u32 = 500;
const FRAMES: u32 = 1700;
const TRIALS: usize = 14;

fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("nes").join(FILE);
    p.exists().then_some(p)
}

fn load_rom(path: &std::path::Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("reads the zip");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip");
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).expect("entry");
        if !entry.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).expect("reads");
        if matches!(
            rf_cart::Cartridge::load(&buf),
            Ok(rf_cart::Cartridge::Nes { .. })
        ) {
            return buf;
        }
    }
    panic!("no NES image in the zip");
}

/// Two Starts, then Right with long hops (W38-02's script).
fn script(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f if (f - 500) % 60 < 34 => RIGHT | A,
        _ => RIGHT,
    }
}

fn inputs() -> Vec<InputFrame> {
    (0..FRAMES)
        .map(|f| {
            let mut i = InputFrame::empty();
            i.ports[0] = u16::from(script(f));
            i
        })
        .collect()
}

/// Test-side state: keep the invincibility timer up in play (never shipped).
fn poke(f: usize, core: &mut NesCore) {
    if f as u32 >= LEAD_IN {
        core.bus_mut().write(0x079F, 0x20);
    }
}

struct Null;
impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn coverage(raw: &[u8]) -> CoverageLog {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for (f, input) in inputs().iter().enumerate() {
        poke(f, &mut core);
        core.run_frame(input, &mut Null);
    }
    core.take_coverage().expect("armed")
}

fn atlas_of(raw: &[u8]) -> Atlas {
    build_nes_atlas(raw, &coverage(raw)).expect("atlas")
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR); a few minutes in release"]
fn smb_widescreen_candidates_and_lockstep_trials() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let t0 = Instant::now();
    let atlas = atlas_of(&raw);
    println!(
        "atlas: {} functions in {:?}",
        atlas.functions.len(),
        t0.elapsed()
    );

    let recipe = rf_profiles::load_recipe_file(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles/nes/super-mario-bros/recipe.toml"),
    )
    .expect("recipe");
    // 1. The trace: object table, camera, HUD sprites.
    let mut core = NesCore::from_ines_bytes(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    nes_prepare(&mut core);
    let mut rec = Recorder::new(Console::Nes, false);
    for (f, input) in inputs().iter().enumerate() {
        poke(f, &mut core);
        let cap = nes_step_frame(&mut core, input);
        if (LEAD_IN as usize - 60..).contains(&f) {
            rec.push(cap, nes_cam(&core));
        }
    }
    let trace = rec.trace;
    let found = infer_tables(&trace);
    let table = to_row(&found[0], rf_rebuild::Grade::Candidate, "; SMB 1-1");
    let table = slots_from_rows(&table, &recipe.ram);
    println!(
        "table base {:#x} slots {} stride {} x_lo {:?} x_hi {:?} state {:?}",
        table.base, table.slots, table.stride, table.x_lo, table.x_hi, table.state
    );
    println!("slots extended to {}", table.slots);
    if std::env::var("CAMDBG").is_ok() {
        let n = trace.frames.len();
        for a in [0x71c_usize, 0x73f, 0x71a, 0x6d, 0x86] {
            let m = trace
                .frames
                .iter()
                .filter(|f| f.ram.get(a).copied() == Some((f.cam_x & 0xFF) as u8))
                .count();
            println!(
                "dbg addr {a:#x}: {m}/{n} ram len {}",
                trace.frames[0].ram.len()
            );
        }
        println!(
            "dbg cams {:?}",
            trace
                .frames
                .iter()
                .step_by(100)
                .map(|f| (f.cam_x, f.ram[0x71c], f.ram[0x73f]))
                .collect::<Vec<_>>()
        );
    }
    let cam = find_camera(&trace).expect("a camera");
    println!("camera lo {:#x} hi {:?}", cam.lo, cam.hi);
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

    let ins = inputs();
    let plain = nes_snaps(&raw, &ins, poke).expect("plain run");
    let mut watch = Watch::default();
    for i in 0..table.slots {
        for off in [table.x_lo, table.x_hi].into_iter().flatten() {
            watch.obj_x.insert(table.base + off + i * table.stride);
        }
        if let Some(off) = table.state {
            watch.obj_state.insert(table.base + off + i * table.stride);
        }
    }
    watch.cam.insert(cam.lo as u32);
    watch.cam.extend(cam.hi.map(|h| h as u32));
    let aliases = camera_aliases(&trace, &cam);
    println!("camera aliases {aliases:x?}");
    watch.cam.extend(aliases.iter().map(|&a| a as u32));
    if let Some(sh) = shadow {
        watch.hud = hud_watch(&trace, sh, &statics);
    }

    // 2. The observed run.
    let t = Instant::now();
    let facts =
        observe_nes(&raw, &atlas, &inputs(), LEAD_IN as usize, watch, poke).expect("observed run");
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

    // Ground truth: the three W36-06 bytes.
    let known = resolve_patches(&atlas, &recipe, "live-enemies").expect("the mod resolves");
    let mut found_known = 0;
    for k in &known {
        let rank = cands.iter().position(|c| c.operand == k.prg_offset);
        println!(
            "KNOWN {} prg {:#x}: candidate rank {:?} {:?}",
            k.name,
            k.prg_offset,
            rank,
            rank.map(|r| (cands[r].kind.name(), cands[r].band.name()))
        );
        found_known += usize::from(rank.is_some());
    }
    assert_eq!(
        found_known,
        known.len(),
        "the W36-06 constants are candidates"
    );

    // 3. Trials: the best candidates plus the known three.
    if std::env::var("SLOTDBG").is_ok() {
        for i in 0..8usize {
            let mv = |a: usize| {
                plain
                    .windows(2)
                    .filter(|w| w[0].ram[a] != w[1].ram[a])
                    .count()
            };
            println!(
                "dbg slot {i}: flag {:#x} changes {} x {:#x} changes {}",
                0xe + i,
                mv(0xe + i),
                0x86 + i,
                mv(0x86 + i)
            );
        }
    }
    let x_page_zero = false;
    let view = View {
        ram_base: 0,
        table: table.clone(),
        cam_lo: cam.lo,
        cam_hi: cam.hi,
        x_page_zero,
    };
    let mut chosen: Vec<&Candidate> = cands
        .iter()
        .filter(|c| c.kind != Kind::ScreenConst && !widen_values(c).is_empty())
        .take(TRIALS)
        .collect();
    for k in &known {
        if let Some(c) = cands.iter().find(|c| c.operand == k.prg_offset) {
            if !chosen.iter().any(|x| x.site == c.site) {
                chosen.push(c);
            }
        }
    }
    if let Ok(only) = std::env::var("ONLY") {
        chosen.retain(|c| format!("{:x}", c.operand) == only);
    }
    let t = Instant::now();
    let tried = try_candidates(
        &TrialSetup {
            raw: &raw,
            view: &view,
            plain: &plain,
            patch: &|r, at, b| nes_patch(r, at, b),
            run: &|image| nes_snaps(image, &ins, poke),
            values: 2,
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
    // W38-04b: the HUD-anchor candidates. Each stored coordinate byte is moved
    // by 16 px on a copy; kept only when nothing but that sprite coordinate
    // differs for the whole run. (NES OAM entry: y, tile, attr, x.)
    let huds: Vec<&Candidate> = cands.iter().filter(|c| c.kind == Kind::HudAnchor).collect();
    let hud_tried = try_hud(
        &TrialSetup {
            raw: &raw,
            view: &view,
            plain: &plain,
            patch: &|r, at, b| nes_patch(r, at, b),
            run: &|image| nes_snaps(image, &ins, poke),
            values: 1,
        },
        &huds,
        oam_shadow(&trace).expect("an OAM shadow"),
        3,
        &mut |l| println!("{l}"),
    )
    .expect("hud trials");
    for h in &hud_tried {
        println!(
            "HUDRESULT rom {:#07x} sprite byte {:?} x {} original {:?} grade {} {}",
            h.cand.operand,
            h.sprite_byte,
            h.is_x,
            h.original,
            h.grade().as_str(),
            h.why_not()
        );
    }
    println!(
        "HUD KEEPERS {:x?} of {} trialed",
        hud_tried
            .iter()
            .filter(|h| h.kept().is_some())
            .map(|h| h.cand.operand)
            .collect::<Vec<_>>(),
        hud_tried.len()
    );
    let skip: Vec<u32> = known.iter().map(|k| k.prg_offset).collect();
    let mut toml = hud_rows_toml(
        &atlas,
        &recipe,
        &hud_tried,
        "widescreen-hud",
        "widescreen-candidates",
        &skip,
        "SMB 1-1 play",
    );
    let toml_objects = rows_toml(
        &atlas,
        &recipe,
        &tried,
        "widescreen-objects",
        "widescreen-candidates",
        &skip,
        "SMB 1-1 play",
    );
    toml = format!("{toml_objects}{toml}");
    if let Ok(out) = std::env::var("W3804_ROWS_OUT") {
        std::fs::write(out, &toml).expect("writes the rows");
    }
    let keepers: Vec<u32> = tried
        .iter()
        .filter(|t| t.grade() == rf_rebuild::Grade::Confirmed)
        .map(|t| t.cand.operand)
        .collect();
    println!("KEEPERS {keepers:x?}");
    if std::env::var("ONLY").is_ok() {
        return;
    }
    // The committed overlay (generated by an earlier run of this test) resolves
    // against this atlas: keepers place, candidates are refused.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/nes/super-mario-bros");
    let merged = rf_profiles::load_recipe_str(&format!(
        "{}\n{}",
        std::fs::read_to_string(dir.join("recipe.toml")).expect("recipe"),
        std::fs::read_to_string(dir.join("recipe-widescreen.toml")).expect("overlay")
    ))
    .expect("merged recipe");
    let placed = resolve_patches(&atlas, &merged, "widescreen-objects").expect("keepers place");
    println!(
        "overlay keepers placed at {:x?}",
        placed.iter().map(|p| p.prg_offset).collect::<Vec<_>>()
    );
    assert!(placed.iter().any(|p| p.prg_offset == 0x41e2));
    assert!(matches!(
        resolve_patches(&atlas, &merged, "widescreen-candidates"),
        Err(rf_rebuild::PatchRefusal::Candidate { .. })
    ));
    assert!(
        keepers.contains(&known[0].prg_offset) || keepers.iter().any(|k| skip.contains(k)),
        "the known activation lead is a lockstep keeper"
    );
}
