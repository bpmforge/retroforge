//! **W36-09: the understand pass and rule names on Super Mario World,
//! Yoshi's Island 1** (ROM-gated, law 5).
//!
//! The same three steps as `rf-harness/tests/w36_09_naming_smb.rs` on the
//! W36-08b session (menus to the level, the W33-07 play script, jump-and-run
//! to 1,500 level frames): record, observe and name by rules, then run the
//! lockstep checker with every proposed name hooked and the hook sink doing
//! nothing. Only names, counts and evidence sentences are printed (D-021).
//!
//! ```text
//! cargo test --release -p retroforge --test w36_09_naming_smw -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::lockstep::Options;
use rf_harness::understand;
use rf_input::SnesButton;
use rf_snes::core::SnesCore;

/// Only for the shared script constants.
#[allow(dead_code, unused_imports)]
#[path = "w33_07_common/mod.rs"]
mod discovery;

const FILE: &str = "Super Mario World (USA).zip";
const PLAY_FRAMES: usize = 1500;

fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join("snes").join(FILE);
    p.exists().then_some(p)
}

/// `wN` idle N frames, `<buttons>N` hold N frames (`A B X Y S E U D L R`
/// joined by `+`), as `Rig::script` reads them.
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
                    "A" => SnesButton::A.bit(),
                    "B" => SnesButton::B.bit(),
                    "X" => SnesButton::X.bit(),
                    "Y" => SnesButton::Y.bit(),
                    "S" => SnesButton::Start.bit(),
                    "E" => SnesButton::Select.bit(),
                    "U" => SnesButton::Up.bit(),
                    "D" => SnesButton::Down.bit(),
                    "L" => SnesButton::Left.bit(),
                    "R" => SnesButton::Right.bit(),
                    other => panic!("button {other}"),
                };
                m | (1 << bit)
            });
            (mask, n.parse().expect("N"))
        };
        out.extend(std::iter::repeat_n(mask, n));
    }
    out
}

fn session(play: &str, more: &str) -> Vec<u16> {
    let to_level = discovery::smw_level_pad();
    let level_start = to_level.len();
    let mut pad = to_level;
    pad.extend(frames_of(play));
    let more = frames_of(more);
    while pad.len() < level_start + PLAY_FRAMES {
        pad.extend(&more);
    }
    pad
}

fn inputs(pad: &[u16]) -> Vec<InputFrame> {
    pad.iter()
        .map(|&mask| {
            let mut f = InputFrame::empty();
            f.ports[0] = mask;
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
#[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR); minutes in release"]
fn smw_yoshis_island_one_names_by_rules_and_inert_hooks_keep_lockstep_green() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let pad = session(discovery::SMW_PLAY, "R+B14 R22");

    // Record, build the atlas.
    let mut core = SnesCore::load(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    for &mask in &pad {
        core.system_mut().bus.joypads.ports[0] = mask;
        core.run_frame(&InputFrame::empty(), &mut Null);
    }
    let log = core.take_coverage().expect("armed");
    let atlas = rf_harness::atlas::build_snes_atlas(&raw, &log).expect("atlas");
    println!("atlas: {} functions", atlas.functions.len());

    let t = Instant::now();
    let run_out = understand::run_snes(&raw, &atlas, inputs(&pad)).expect("observes");
    println!("observed in {:?}", t.elapsed());
    for l in understand::summary_lines(&run_out, &atlas) {
        println!("{l}");
    }
    println!("| name | source | conf | evidence |");
    for l in understand::sample_lines(&run_out) {
        println!("{l}");
    }
    // The observer's frame loop is the game's: same end state as run_frame.
    let all_inputs = inputs(&pad);
    assert_eq!(
        understand::observed_matches_plain_snes(&raw, &atlas, &all_inputs),
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

    // No SMW recipe ships yet, so the model would see no RAM names here.
    audit_ai_payload(&raw, &atlas, &run_out, &[]);

    let base = rf_rebuild::Recipe {
        id: "smw-names".into(),
        engine: "smw".into(),
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

    let opts = Options::<SnesCore> {
        function_hooks: ids,
        hook_sink: Some(understand::inert_sink()),
        ..Options::default()
    };
    let r = rf_harness::lockstep_snes::run(&raw, &atlas, inputs(&pad), opts).expect("runs");
    println!(
        "lockstep with hooks: {} frames, divergence {:?}, {:.3}% fallback",
        r.frames_run,
        r.divergence.as_ref().map(|d| d.frame),
        r.stats.fallback_rate() * 100.0
    );
    assert_eq!(r.divergence.as_ref().map(|d| d.frame), None);
    assert_eq!(r.frames_run as usize, pad.len());
}
