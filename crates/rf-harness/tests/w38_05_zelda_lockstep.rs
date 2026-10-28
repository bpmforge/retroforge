//! **W38-05: lockstep on The Legend of Zelda** (ROM-gated, law 5).
//!
//! The scripted play of `w38_05_zelda_common` (file registration, the first
//! cave, then the overworld in three directions) runs on the plain core
//! (reference) and on the rebuilt core after every frame, with zero
//! divergence required: first on the unmodified image, then on the image with
//! the recipe's widescreen keepers applied (when the overlay has any).
//!
//! Only counts and hashes are printed; no ROM bytes leave the test (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_05_zelda_lockstep -- --ignored --nocapture
//! RF_W36_TIER=ir cargo test --release -p rf-harness --test w38_05_zelda_lockstep -- --ignored --nocapture
//! ```

mod w38_05_zelda_common;

use std::collections::HashSet;

use rf_core_api::{EmulatorCore, RebuildStats, ResetKind};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::lockstep::{run, Options, Side};
use rf_nes::core::NesCore;
use w38_05_zelda_common::{inputs, load_rom, rom_path, NullSink, FILE, FRAMES};

fn record(raw: &[u8]) -> rf_core_api::coverage::CoverageLog {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for input in inputs(FRAMES) {
        core.run_frame(&input, &mut NullSink);
    }
    core.take_coverage().expect("armed")
}

fn rebuilt_tier() -> rf_nes::ExecTier {
    if std::env::var("RF_W36_TIER").is_ok_and(|v| v == "ir") {
        rf_nes::ExecTier::Ir
    } else {
        rf_nes::ExecTier::Interpreter
    }
}

fn nametable_print(core: &NesCore) -> u64 {
    core.state_view()
        .vram
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// Runs the lockstep of one image; returns (frames, divergence, stats, distinct
/// nametable prints sampled every 16 frames).
fn lockstep(raw: &[u8], tag: &str) -> (u32, Option<u32>, RebuildStats, usize) {
    let atlas = build_nes_atlas(raw, &record(raw)).expect("atlas");
    println!("{tag}: atlas {} functions", atlas.functions.len());
    let tier = rebuilt_tier();
    let mut prints: HashSet<u64> = HashSet::new();
    let dir = std::env::temp_dir().join(format!("rf-w38-05-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut hook = |frame: u32, side: Side, core: &mut NesCore| {
        if side == Side::Rebuilt {
            core.set_exec_tier(tier);
            assert_eq!(core.exec_tier(), tier, "the rebuilt core runs the tier");
            if frame % 16 == 0 {
                prints.insert(nametable_print(core));
            }
        }
    };
    let opts = Options {
        bundle_dir: Some(dir),
        before_frame: Some(&mut hook),
        ..Options::default()
    };
    let r = run(raw, &atlas, inputs(FRAMES), opts).expect("lockstep runs");
    (
        r.frames_run,
        r.divergence.as_ref().map(|d| d.frame),
        r.stats,
        prints.len(),
    )
}

#[test]
#[ignore = "needs The Legend of Zelda (USA) (RF_SCREENS_ROMS_DIR); about a minute in release"]
fn zelda_plain_image_has_zero_divergence() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let (frames, div, stats, prints) = lockstep(&raw, "plain");
    println!(
        "plain image: {frames} frames, divergence {div:?}, {:.3}% fallback ({} instrs), {prints} distinct nametable states",
        stats.fallback_rate() * 100.0,
        stats.instructions()
    );
    assert_eq!(div, None);
    assert_eq!(frames, FRAMES);
    assert!(prints >= 8, "the script crossed screens ({prints})");
}
