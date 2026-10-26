//! **W36-10: the Super Mario World recipe** (`profiles/snes/super-mario-world/
//! recipe.toml`), regenerated from observation and compared with the committed
//! file (ROM-gated, law 5).
//!
//! The rows are the W36-09 rule names on the Yoshi's Island 1 run (shape hash,
//! exact hash, address hint and the **entry mode**) plus RAM meaning rows from
//! the SMW profile's observed facts. No bytes are stored (D-021). Regenerating
//! must reproduce the committed rows exactly; every row must resolve, exactly
//! and to the function of the right mode, on that same atlas.
//!
//! ```text
//! cargo test --release -p retroforge --test w36_10_smw_recipe -- --ignored --nocapture
//! RF_W36_10_WRITE_RECIPE=1 ...   # (re)write the committed file
//! ```

use std::path::PathBuf;

use rf_core_api::{EmulatorCore, ResetKind};
use rf_harness::understand;
use rf_rebuild::{match_recipe, Confidence, RamMeaning, Recipe};
use rf_snes::core::SnesCore;

mod w36_10_common;
use w36_10_common::*;

const HEADER: &str = "\
# Super Mario World (USA): function names and RAM meanings, built from
# observation (ticket W36-10, D-021: no bytes, no disassembly).
#
# Function rows are the W36-09 rule names (`rule:<name>` sources) on a
# Yoshi's Island 1 run: the shape hash survives a ROM hack that moves the
# function, the exact hash raises the match to exact, and `mode` is the 65816
# decode mode (0 emulation, 1..=4 native, 1 + m8 + 2 * x8) the function is
# entered in -- the shape hash ignores it, so a SNES row matches on (shape,
# mode). RAM rows come from profile.toml's observed facts. Regenerate with
# crates/retroforge/tests/w36_10_smw_recipe.rs (RF_W36_10_WRITE_RECIPE=1).

";

fn recipe_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/snes/super-mario-world/recipe.toml")
}

#[test]
#[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR); minutes in release"]
fn regenerating_the_smw_recipe_reproduces_the_committed_rows() {
    let Some(path) = smw_path() else {
        println!("SKIP: {SMW_FILE} not found");
        return;
    };
    let raw = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let pad = smw_session();

    let mut core = SnesCore::load(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    for &mask in &pad {
        core.system_mut().bus.joypads.ports[0] = mask;
        core.run_frame(&rf_core_api::InputFrame::empty(), &mut Null);
    }
    let log = core.take_coverage().expect("armed");
    let atlas = rf_harness::atlas::build_snes_atlas(&raw, &log).expect("atlas");
    let run_out = understand::run_snes(&raw, &atlas, inputs(&pad)).expect("observes");

    let base = Recipe {
        id: "super-mario-world".into(),
        engine: "smw".into(),
        ram: SMW_RAM
            .iter()
            .map(|&(addr, width, name, src)| RamMeaning {
                addr,
                width,
                name: name.into(),
                source: src.into(),
                // profile.toml marks `lives` verified (0x04 at a new file).
                grade: if name == "lives" {
                    rf_rebuild::Grade::Confirmed
                } else {
                    rf_rebuild::Grade::Observed
                },
            })
            .collect(),
        ..Recipe::default()
    };
    let (generated, ids) =
        understand::name_hooks(&atlas, &run_out.proposals, base).expect("every row resolves");
    println!(
        "{} atlas functions, {} name rows, {} RAM rows",
        atlas.functions.len(),
        generated.functions.len(),
        generated.ram.len()
    );
    assert_eq!(ids.len(), generated.functions.len());
    assert!(generated.functions.iter().all(|f| f.mode.is_some()));
    for want in ["reset", "nmi"] {
        assert!(generated.functions.iter().any(|f| f.name == want), "{want}");
    }

    let committed_path = recipe_path();
    if std::env::var("RF_W36_10_WRITE_RECIPE").is_ok() {
        let text = format!(
            "{HEADER}{}",
            rf_profiles::recipe::recipe_to_toml(&generated)
        );
        std::fs::write(&committed_path, text).expect("writes the recipe");
        println!("wrote {}", committed_path.display());
    }
    let committed = rf_profiles::load_recipe_file(&committed_path)
        .expect("the committed recipe (run once with RF_W36_10_WRITE_RECIPE=1)");
    assert_eq!(
        committed, generated,
        "regenerating reproduces the committed recipe"
    );

    // Every committed row resolves exactly, to the function of its mode.
    let report = match_recipe(&atlas, &committed);
    assert!(report.unmatched.is_empty(), "{:?}", report.unmatched);
    for m in &report.matched {
        let row = committed
            .functions
            .iter()
            .find(|f| f.name == m.name)
            .expect("row");
        let f = &atlas.functions[m.function as usize];
        assert_eq!(m.confidence, Confidence::Exact, "{}", m.name);
        assert_eq!(Some(f.entry_mode), row.mode, "{}", m.name);
    }
    println!("all {} rows exact on their own atlas", report.matched.len());
}
