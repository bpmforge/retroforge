//! Shared by the W36-06b tests: the SMB multicart, the live-enemies image and the scripts of
//! `rf-harness/tests/w36_06_live_enemies.rs` (ROM-gated, law 5).
#![allow(dead_code)]

use std::io::Read as _;
use std::path::PathBuf;

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_enhance::patch::prg::apply_prg_bytes;
use rf_harness::atlas::build_nes_atlas;
use rf_nes::core::NesCore;
use rf_rebuild::{resolve_patches, Atlas, Recipe};

pub const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
pub const START: u8 = 1 << 3;
pub const A: u8 = 1 << 0;
pub const B: u8 = 1 << 1;
pub const RIGHT: u8 = 1 << 7;
pub const MOD: &str = "live-enemies";

pub fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("nes").join(FILE);
    p.exists().then_some(p)
}

pub fn load_rom(path: &std::path::Path) -> Vec<u8> {
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

/// Two Starts, then Right with A pressed 12 frames in every 60.
pub fn script_one(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f if (f - 500) % 60 < 12 => RIGHT | A,
        _ => RIGHT,
    }
}

/// Run the whole way, jump 20 frames in every 90.
pub fn script_two(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f => {
            let t = f - 500;
            let jump = t % 90 < 20 || t % 150 < 6;
            RIGHT | B | if jump { A } else { 0 }
        }
    }
}

pub fn input(buttons: u8) -> InputFrame {
    let mut i = InputFrame::empty();
    i.ports[0] = u16::from(buttons);
    i
}

struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

pub fn recipe() -> Recipe {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/nes/super-mario-bros/recipe.toml");
    rf_profiles::load_recipe_file(&path).expect("the SMB recipe parses")
}

/// The atlas the app would have after 700 recorded frames of 1-1.
pub fn atlas_of(raw: &[u8]) -> Atlas {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for f in 0..700 {
        core.run_frame(&input(script_one(f)), &mut NullSink);
    }
    let log: CoverageLog = core.take_coverage().expect("armed");
    build_nes_atlas(raw, &log).expect("atlas")
}

/// The live-enemies image (the mod's rows resolved against the unpatched atlas).
pub fn mod_image(raw: &[u8]) -> Vec<u8> {
    let patches = resolve_patches(&atlas_of(raw), &recipe(), MOD).expect("the mod resolves");
    let edits: Vec<(u32, u8)> = patches.iter().map(|p| (p.prg_offset, p.value)).collect();
    apply_prg_bytes(raw, &edits).expect("applies")
}
