//! **W38-09: the ROM-hack gate** (ROM-gated, law 5 / D-024). Twenty random
//! Lunar Magic Super Mario World hacks, patched IN MEMORY onto the user's own
//! SMW image (the patch zips are the user's downloads under
//! `<roms>/snes/hacks/w38/NN/patch.zip`; no patched ROM or patch byte is ever
//! written anywhere, and nothing is named: hacks are `01..20`).
//!
//! Per hack, with no per-hack data of any kind:
//! 1. apply the `.bps` / `.ips` from the zip with `rf_enhance::patch`;
//! 2. boot it on a plain core with a generic Start / A drive (any game
//!    mode reaching a level, the map, or at least attract is reported),
//!    recording coverage, then build the generic atlas from that run;
//! 3. rebuilt versus plain lockstep over the first `LOCKSTEP_FRAMES` frames
//!    of that drive: the rebuilt tier must boot with zero divergence;
//! 4. match `profiles/snes/super-mario-world/recipe.toml` by hash
//!    (`match_recipe`), counting matched / total recipe functions;
//! 5. the live margins: `recipe-widescreen.toml` is appended to the recipe and
//!    every mod's rows are resolved against the hack's atlas
//!    (`resolve_patches`, all-or-nothing, candidates never apply). SMW has no
//!    keeper widescreen rows yet (W38-04: candidates only), so the table says
//!    how many candidate rows WOULD land (their function matched exactly)
//!    and how many mods actually resolved.
//!
//! Only offsets and counts are printed.
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_09_hack_gate -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_snes_atlas;
use rf_harness::lockstep::Options;
use rf_input::SnesButton;
use rf_rebuild::{match_recipe, resolve_patches, Confidence, Grade, Recipe};
use rf_snes::core::SnesCore;

const FILE: &str = "Super Mario World (USA).zip";
const GAME_MODE: u32 = 0x7E_0100;
const IN_LEVEL: u8 = 0x14;
const MAP: u8 = 0x0E;
/// Frames of the generic drive (Start / A presses, then play in a level).
const DRIVE_FRAMES: usize = 3_000;
/// Level frames after which the drive stops early.
const PLAY_FRAMES: usize = 400;
/// Frames of the drive that run in lockstep.
const LOCKSTEP_FRAMES: usize = 2_500;

fn roms_root() -> String {
    std::env::var("RF_SCREENS_ROMS_DIR").unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".into())
}

fn hacks_dir() -> PathBuf {
    Path::new(&roms_root())
        .join("snes")
        .join("hacks")
        .join("w38")
}

fn read_zip_entries(bytes: Vec<u8>) -> Vec<(String, Vec<u8>)> {
    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else {
            continue;
        };
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        let mut buf = Vec::new();
        if entry.read_to_end(&mut buf).is_ok() {
            out.push((name, buf));
        }
    }
    out
}

fn load_base() -> Option<Vec<u8>> {
    let p = Path::new(&roms_root()).join("snes").join(FILE);
    let bytes = std::fs::read(p).ok()?;
    read_zip_entries(bytes).into_iter().find_map(|(_, b)| {
        matches!(
            rf_cart::Cartridge::load(&b),
            Ok(rf_cart::Cartridge::Snes { .. })
        )
        .then_some(b)
    })
}

/// The first `.bps` / `.ips` of `<dir>/patch.zip` that applies to `base`
/// (bytes only ever in memory). Returns the patched image.
fn patched(dir: &Path, base: &[u8]) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(dir.join("patch.zip")).map_err(|e| format!("no patch.zip: {e}"))?;
    let mut last = "no .bps/.ips in the zip".to_string();
    for (name, patch) in read_zip_entries(bytes) {
        let lower = name.to_ascii_lowercase();
        if !(lower.ends_with(".bps") || lower.ends_with(".ips")) {
            continue;
        }
        match rf_enhance::patch::apply_patch(base, &patch) {
            Ok((rom, _)) => return Ok(rom),
            Err(e) => last = format!("{e}"),
        }
    }
    Err(last)
}

fn mask(buttons: &str) -> u16 {
    let bit = match buttons {
        "A" => SnesButton::A,
        "B" => SnesButton::B,
        "S" => SnesButton::Start,
        "U" => SnesButton::Up,
        "D" => SnesButton::Down,
        "L" => SnesButton::Left,
        "R" => SnesButton::Right,
        other => panic!("button {other}"),
    }
    .bit();
    1 << bit
}

/// Start and A alternating every 120 frames after a 200-frame wait.
fn boot_mask(f: usize) -> u16 {
    let (slot, p) = (f / 120, f % 120);
    if f < 200 || p >= 6 {
        0
    } else if slot % 2 == 0 {
        mask("S")
    } else {
        mask("A")
    }
}

/// On the overworld: push a direction, wait for the walk, press A.
fn map_mask(frame: usize, visit: usize) -> u16 {
    const DIRS: [&str; 4] = ["L", "R", "U", "D"];
    match frame {
        1..=40 => mask(DIRS[visit % DIRS.len()]),
        161..=168 => mask("A"),
        _ => 0,
    }
}

/// In a level: run right, hop on a beat.
fn play_mask(f: usize) -> u16 {
    match f % 36 {
        0..=13 => mask("R") | mask("B"),
        _ => mask("R"),
    }
}

struct Null;
impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

struct Drive {
    pad: Vec<u16>,
    log: rf_core_api::CoverageLog,
    modes: Vec<u8>,
    in_level: usize,
}

/// Law 8: every pass of the loop appends exactly one frame, and the loop is
/// bounded by `DRIVE_FRAMES`.
fn drive(raw: &[u8]) -> Drive {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    let (mut pad, mut modes) = (Vec::new(), Vec::new());
    let (mut in_level, mut level_f) = (0usize, 0usize);
    let (mut map_frame, mut visits, mut last) = (0usize, 0usize, 0xFFu8);
    while pad.len() < DRIVE_FRAMES && in_level < PLAY_FRAMES {
        let m = core.peek(GAME_MODE);
        if !modes.contains(&m) && modes.len() < 64 {
            modes.push(m);
        }
        if m == MAP && last != MAP {
            visits += 1;
            map_frame = 0;
        }
        last = m;
        let pad_now = if m == IN_LEVEL && pad.len() > 300 {
            in_level += 1;
            level_f += 1;
            play_mask(level_f)
        } else if m == MAP {
            map_frame += 1;
            map_mask(map_frame, visits)
        } else {
            boot_mask(pad.len())
        };
        pad.push(pad_now);
        core.system_mut().bus.joypads.ports[0] = pad_now;
        core.run_frame(&InputFrame::empty(), &mut Null);
    }
    modes.sort_unstable();
    Drive {
        log: core.take_coverage().expect("armed"),
        pad,
        modes,
        in_level,
    }
}

struct Row {
    id: String,
    boots: &'static str,
    divergence: String,
    matched: usize,
    total: usize,
    exact: usize,
    mods_resolved: String,
    candidates_would_apply: String,
    fallback: String,
}

fn with_overlay() -> (Recipe, Recipe) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/snes/super-mario-world");
    let base = std::fs::read_to_string(dir.join("recipe.toml")).expect("recipe");
    let overlay = std::fs::read_to_string(dir.join("recipe-widescreen.toml")).expect("overlay");
    (
        rf_profiles::load_recipe_str(&base).expect("base recipe"),
        rf_profiles::load_recipe_str(&format!("{base}\n{overlay}")).expect("overlay recipe"),
    )
}

fn analyse(id: &str, raw: &[u8], recipe: &Recipe, full: &Recipe) -> Row {
    let t = Instant::now();
    let d = drive(raw);
    let boots = if d.modes.contains(&IN_LEVEL) {
        "level"
    } else if d.modes.contains(&MAP) {
        "map"
    } else {
        "title/attract"
    };
    let atlas = build_snes_atlas(raw, &d.log).expect("atlas");
    let report = match_recipe(&atlas, recipe);
    let exact = report
        .matched
        .iter()
        .filter(|m| m.confidence == Confidence::Exact)
        .count();

    // Margins: every mod of the overlay recipe, resolved against this atlas.
    let full_report = match_recipe(&atlas, full);
    let mut resolved = Vec::new();
    for m in full.mods() {
        match resolve_patches(&atlas, full, m) {
            Ok(rows) => resolved.push(format!("{m}:{}", rows.len())),
            Err(e) => resolved.push(format!("{m}:refused({})", refusal_kind(&e))),
        }
    }
    // Candidate rows whose function matched exactly: what would apply if a
    // keeper trial graduated them.
    let cand: Vec<_> = full
        .patches
        .iter()
        .filter(|p| p.grade == Grade::Candidate)
        .collect();
    let would = cand
        .iter()
        .filter(|p| {
            full_report
                .matched
                .iter()
                .any(|m| m.name == p.function && m.confidence == Confidence::Exact)
        })
        .count();

    let frames = d.pad.len().min(LOCKSTEP_FRAMES);
    let inputs: Vec<InputFrame> = d.pad[..frames]
        .iter()
        .map(|&m| {
            let mut f = InputFrame::empty();
            f.ports[0] = m;
            f
        })
        .collect();
    let opts = Options::<SnesCore> {
        bundle_dir: Some(std::env::temp_dir().join(format!("rf-w38-09-{id}"))),
        ..Options::default()
    };
    let r = rf_harness::lockstep_snes::run(raw, &atlas, inputs, opts).expect("lockstep runs");
    let divergence = match &r.divergence {
        None => format!("none/{}f", r.frames_run),
        Some(dv) => format!(
            "f{} {:?}",
            dv.frame,
            dv.fields.iter().map(|f| f.field).collect::<Vec<_>>()
        ),
    };
    let s = &r.stats;
    println!(
        "[{id}] {:?}: modes {:02x?}, {} level frames, atlas {} fns, recipe {}/{} ({exact} exact)",
        t.elapsed(),
        d.modes,
        d.in_level,
        atlas.functions.len(),
        report.matched.len(),
        recipe.functions.len()
    );
    Row {
        id: id.to_string(),
        boots,
        divergence,
        matched: report.matched.len(),
        total: recipe.functions.len(),
        exact,
        mods_resolved: resolved.join(","),
        candidates_would_apply: format!("{would}/{}", cand.len()),
        fallback: format!("{:.3}%", s.fallback_rate() * 100.0),
    }
}

fn refusal_kind(e: &rf_rebuild::PatchRefusal) -> &'static str {
    use rf_rebuild::PatchRefusal as P;
    match e {
        P::NoSuchMod(_) => "no-mod",
        P::UnknownFunction { .. } => "unknown-fn",
        P::Unmatched { .. } => "unmatched",
        P::NotExact { .. } => "not-exact",
        P::Ambiguous { .. } => "ambiguous",
        P::OutOfFunction { .. } => "out-of-fn",
        P::Overlap { .. } => "overlap",
        P::Candidate { .. } => "candidate",
    }
}

#[test]
#[ignore = "needs Super Mario World (USA) and the twenty hack patch zips (RF_SCREENS_ROMS_DIR); many minutes in release"]
fn twenty_smw_hacks_boot_rebuilt_and_match_the_recipe() {
    let Some(base) = load_base() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    if !hacks_dir().exists() {
        println!("SKIP: {} not found", hacks_dir().display());
        return;
    }
    let (recipe, full) = with_overlay();
    let only = std::env::var("RF_W38_09_ONLY").ok();
    let mut rows = Vec::new();
    let mut skipped = Vec::new();
    for n in 1..=20 {
        let id = format!("{n:02}");
        if only.as_deref().is_some_and(|o| o != id) {
            continue;
        }
        let rom = match patched(&hacks_dir().join(&id), &base) {
            Ok(r) => r,
            Err(e) => {
                println!("[{id}] SKIP: {e}");
                skipped.push(format!("{id}: {e}"));
                continue;
            }
        };
        rows.push(analyse(&id, &rom, &recipe, &full));
    }
    println!("\n| hack | boots | divergence | matched/total (exact) | margins applied (mod:rows) | candidate rows that would apply | fallback |");
    for r in &rows {
        println!(
            "| {} | {} | {} | {}/{} ({}) | {} | {} | {} |",
            r.id,
            r.boots,
            r.divergence,
            r.matched,
            r.total,
            r.exact,
            r.mods_resolved,
            r.candidates_would_apply,
            r.fallback
        );
    }
    println!("skipped: {skipped:?}");
    for r in &rows {
        assert!(
            r.divergence.starts_with("none"),
            "{}: {}",
            r.id,
            r.divergence
        );
    }
    assert!(
        skipped.is_empty(),
        "patches that did not apply: {skipped:?}"
    );
}
