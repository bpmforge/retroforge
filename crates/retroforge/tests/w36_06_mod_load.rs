//! Ticket W36-06: the app's load path for a recipe mod, on real Super Mario
//! Bros. (`#[ignore]`, ROM-gated, law 5).
//!
//! The app opens a game, finds the atlas of the ORIGINAL image in the per-ROM
//! cache (the Rebuild panel stored it), resolves the matched profile's recipe
//! rows against it and applies the mods the player turned on, in memory. The
//! settings, badge and session identity are covered by unit tests in
//! `soft_patch` and `game_settings`; this checks the glue on the real cart:
//! cache lookup, recipe file, resolution and the edited image.
//!
//! ```text
//! cargo test --release -p retroforge --test w36_06_mod_load -- --ignored --nocapture
//! ```

use std::io::Read as _;

use retroforge::rebuild::{cached_atlas, store_atlas_for_test};
use retroforge::soft_patch::{apply_mods, SoftPatchSession};
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";

fn load_rom() -> Option<Vec<u8>> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let bytes = std::fs::read(std::path::Path::new(&root).join("nes").join(FILE)).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).ok()?;
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).ok()?;
        if matches!(
            rf_cart::Cartridge::load(&buf),
            Ok(rf_cart::Cartridge::Nes { .. })
        ) {
            return Some(buf);
        }
    }
    None
}

struct Quiet;
impl CoreSink for Quiet {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn the_app_path_resolves_the_recipe_from_the_cached_atlas_and_edits_three_bytes() {
    let Some(raw) = load_rom() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    // What a player has done before: recorded some 1-1 play, turned Rebuild
    // on once (which stores the atlas under the image's hash).
    let mut core = rf_nes::core::NesCore::from_ines_bytes(&raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for f in 0..700u32 {
        let mut i = InputFrame::empty();
        i.ports[0] = match f {
            200..=204 | 400..=404 => 1 << 3,
            500.. => 1 << 7,
            _ => 0,
        };
        core.run_frame(&i, &mut Quiet);
    }
    let atlas =
        rf_harness::atlas::build_nes_atlas(&raw, &core.take_coverage().unwrap()).expect("atlas");
    let sha = retroforge::hash::sha256_hex(&raw);
    store_atlas_for_test(&sha, &atlas);
    let cached = cached_atlas(&sha).expect("the stored atlas is found");
    assert_eq!(cached.functions.len(), atlas.functions.len());

    let recipe_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/nes/super-mario-bros/recipe.toml");
    let recipe = rf_profiles::load_recipe_file(&recipe_path).expect("recipe");
    let enabled = vec!["live-enemies".to_string()];

    // Off: the image is the input.
    let off = apply_mods(raw.clone(), Some(&cached), &recipe, &[]);
    assert_eq!(off.image, raw);
    // On: three bytes of the copy.
    let on = apply_mods(raw.clone(), Some(&cached), &recipe, &enabled);
    assert!(on.failures.is_empty(), "{:?}", on.failures);
    assert_eq!(on.applied, ["live-enemies"]);
    let changed = (0..raw.len()).filter(|&i| raw[i] != on.image[i]).count();
    assert_eq!(changed, 3);
    // No cached atlas: refused with the reason, never guessed.
    let none = apply_mods(raw.clone(), None, &recipe, &enabled);
    assert!(none.applied.is_empty() && none.image == raw);
    println!("{}", none.failures[0]);

    let mut session = SoftPatchSession::unpatched(&sha);
    session.mods = on.applied;
    session.session_sha256 = retroforge::hash::sha256_hex(&on.image);
    assert!(!session.is_reference());
    assert_ne!(session.session_sha256, session.base_sha256);
    println!("badge: {}", session.badge().unwrap());
    // The patched image boots.
    let mut patched = rf_nes::core::NesCore::from_ines_bytes(&on.image).expect("boots");
    patched.reset(ResetKind::Hard);
    for _ in 0..30 {
        patched.run_frame(&InputFrame::empty(), &mut Quiet);
    }
}
