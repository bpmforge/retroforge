//! **W36-09: the understand pass and rule names on Super Mario Bros., World
//! 1-1** (ROM-gated, law 5).
//!
//! 1. Record the 1-1 drive script, build the atlas.
//! 2. Run it again under the observer (every function hooked, every
//!    instruction and bus access attributed) and propose names with the
//!    rules. Prints the counts and a sample (the numbers in
//!    `docs/research/W36-09-naming.md`); only names, counts and evidence
//!    sentences leave the test, never ROM bytes (D-021).
//! 3. Export the names as recipe rows, round-trip them through the recipe
//!    TOML, resolve them against the atlas and run the lockstep checker with
//!    **every proposed name hooked** and the hook sink doing nothing: rebuilt
//!    and plain must agree over the whole run.
//!
//! ```text
//! cargo test --release -p rf-harness --test w36_09_naming_smb -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;
use std::time::Instant;

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::lockstep::{run, Options};
use rf_harness::understand;
use rf_nes::core::NesCore;

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u8 = 1 << 3;
const A: u8 = 1 << 0;
const RIGHT: u8 = 1 << 7;
/// 500 frames to get through both title screens plus the intro, then play.
const LEAD_IN: u32 = 500;
const PLAY: u32 = 1200;
const FRAMES: u32 = LEAD_IN + PLAY;

fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("nes").join(FILE);
    p.exists().then_some(p)
}

/// The single NES image inside the zip (content-sniffed, like the app).
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

/// Two Starts (game select, then SMB's title), then Right with A pressed 12
/// frames in every 60. Same as `w36_03_rebuild_smb`'s script one.
fn script_one(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f if (f - 500) % 60 < 12 => RIGHT | A,
        _ => RIGHT,
    }
}

/// Run (B) the whole way, jump 20 frames in every 90 and hop at the start of
/// every 150. Same as `w36_03_rebuild_smb`'s script two.
fn inputs(script: fn(u32) -> u8) -> Vec<InputFrame> {
    (0..FRAMES)
        .map(|f| {
            let mut i = InputFrame::empty();
            i.ports[0] = u16::from(script(f));
            i
        })
        .collect()
}

struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// Record a run of `script` (entering `level`) and return its coverage.
fn record(raw: &[u8], script: fn(u32) -> u8) -> CoverageLog {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for input in inputs(script) {
        core.run_frame(&input, &mut NullSink);
    }
    core.take_coverage().expect("armed")
}

/// The opt-in AI pass's payload for this run, audited against the real
/// cartridge: behaviour facts only (D-024).
fn audit_ai_payload(
    raw: &[u8],
    atlas: &rf_rebuild::Atlas,
    run_out: &understand::NamingRun,
    ram: &[rf_rebuild::RamMeaning],
) {
    use rf_ai::naming::{
        audit_request, build_requests, closed_vocabulary, AuditInput, BatchOptions,
    };
    let opts = BatchOptions {
        per_request: 40,
        max_requests: 64,
        include_named: true,
    };
    let requests = build_requests(&run_out.understanding, atlas, &run_out.proposals, ram, opts);
    let mut allowed = closed_vocabulary();
    allowed.extend(ram.iter().map(|r| r.name.clone()));
    allowed.extend(
        run_out
            .proposals
            .iter()
            .flat_map(|p| [p.name.clone(), p.base.clone()]),
    );
    let hashes: Vec<u64> = atlas
        .functions
        .iter()
        .flat_map(|f| [f.byte_hash, f.shape_hash])
        .collect();
    let mut asked = 0;
    for r in &requests {
        audit_request(
            r,
            &AuditInput {
                rom: raw,
                hashes: &hashes,
                allowed_strings: &allowed,
            },
        )
        .expect("the payload is behaviour facts only");
        asked += r.functions.len();
    }
    println!(
        "AI payload: {} request(s), {asked} function(s), audited against {} ROM bytes: clean",
        requests.len(),
        raw.len()
    );
    assert!(asked > 0);
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR); minutes in release"]
fn smb_1_1_names_by_rules_and_inert_hooks_keep_lockstep_green() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let log = record(&raw, script_one);
    let atlas = build_nes_atlas(&raw, &log).expect("atlas");

    let t = Instant::now();
    let run_out = understand::run_nes(&raw, &atlas, inputs(script_one)).expect("observes");
    println!("observed in {:?}", t.elapsed());
    for l in understand::summary_lines(&run_out, &atlas) {
        println!("{l}");
    }
    println!("| name | source | conf | evidence |");
    for l in understand::sample_lines(&run_out) {
        println!("{l}");
    }
    // The observer's frame loop is the game's: same end state as run_frame.
    let all_inputs: Vec<InputFrame> = inputs(script_one);
    assert_eq!(
        understand::observed_matches_plain_nes(&raw, &atlas, &all_inputs),
        Ok(true),
        "the observed run ends where a plain run ends"
    );
    println!("observed run == plain run over {} frames", all_inputs.len());
    let names: Vec<&str> = run_out.proposals.iter().map(|p| p.name.as_str()).collect();
    for want in ["reset", "nmi"] {
        assert!(names.contains(&want), "{want} not named: {names:?}");
    }
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "names are unique");

    let recipe_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/nes/super-mario-bros/recipe.toml");
    let ram = rf_profiles::load_recipe_file(&recipe_path)
        .expect("the SMB recipe")
        .ram;
    audit_ai_payload(&raw, &atlas, &run_out, &ram);

    // Export, round trip, resolve.
    let base = rf_rebuild::Recipe {
        id: "smb-names".into(),
        engine: "smb1".into(),
        ..rf_rebuild::Recipe::default()
    };
    let (recipe, ids) = understand::name_hooks(&atlas, &run_out.proposals, base).expect("resolves");
    assert!(recipe.functions.len() <= run_out.proposals.len());
    assert_eq!(
        ids.len(),
        recipe.functions.len(),
        "every row resolves to its own function"
    );
    let toml = rf_profiles::recipe::recipe_to_toml(&recipe);
    assert_eq!(
        rf_profiles::recipe::load_recipe_str(&toml).expect("reads back"),
        recipe
    );
    println!(
        "{} name rows -> {} hooked functions",
        recipe.functions.len(),
        ids.len()
    );

    // Lockstep with every proposed name hooked and inert.
    let opts = Options {
        function_hooks: ids,
        hook_sink: Some(understand::inert_sink()),
        ..Options::default()
    };
    let r = run(&raw, &atlas, inputs(script_one), opts).expect("lockstep runs");
    println!(
        "lockstep with hooks: {} frames, divergence {:?}, {:.3}% fallback",
        r.frames_run,
        r.divergence.as_ref().map(|d| d.frame),
        r.stats.fallback_rate() * 100.0
    );
    assert_eq!(r.divergence.as_ref().map(|d| d.frame), None);
    assert_eq!(r.frames_run, FRAMES);
}
