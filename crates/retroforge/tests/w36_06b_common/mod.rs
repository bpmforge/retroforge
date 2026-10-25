//! Shared by the W36-06b tests: the SMB multicart, the live-enemies image and the scripts of
//! `rf-harness/tests/w36_06_live_enemies.rs` (ROM-gated, law 5).
#![allow(dead_code)]

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::stepper::EmuStepper;

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

// ---- shared by the 06b and 06c screens tests ----

pub struct Discard;

impl CoreSink for Discard {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// Camera-relative x of each active enemy slot.
pub fn rels(st: &EmuStepper) -> Vec<i32> {
    let cam = i32::from(st.peek_bus(0x071A)) * 256 + i32::from(st.peek_bus(0x071C));
    (0..5u32)
        .filter(|k| st.peek_bus(0x0F + k) != 0)
        .map(|k| i32::from(st.peek_bus(0x6E + k)) * 256 + i32::from(st.peek_bus(0x87 + k)) - cam)
        .collect()
}

/// Enemies deep in the right margin, deep in the left, and in the picture.
pub fn census(st: &EmuStepper) -> (usize, usize, usize) {
    let r = rels(st);
    (
        r.iter().filter(|r| (300..480).contains(*r)).count(),
        r.iter().filter(|r| (-250..-100).contains(*r)).count(),
        r.iter().filter(|r| (24..232).contains(*r)).count(),
    )
}

/// Play the unhurt script on the live-enemies image to a frame where enemies stand in both margins and
/// still do `idle_frames` later with the pad idle and Mario alive; return the state container there.
///
/// `idle_frames`: 45 for a screenshot of one frame; W36-06c needs the scene to hold while the level
/// scout runs. `unhurt_idle`: Mario's injury timer is kept up through the idle frames too (and the
/// saved state holds it at its top), so he is alive however long the game then runs idle. `camera`: the
/// world x the scene's camera has to be in (the level scout only reaches so far).
pub fn find_scene(
    image: &[u8],
    session: &retroforge::mod_chunk::ModSession,
    idle_frames: u32,
    unhurt_idle: bool,
    camera: std::ops::RangeInclusive<i32>,
) -> rf_state::Container {
    let mut st = EmuStepper::open(image).expect("opens");
    let mut best: Option<((usize, usize, usize), rf_state::Container)> = None;
    for f in 0..3200u32 {
        let inp = input(script_two(f));
        if st.peek_bus(0x0770) == 1 {
            st.poke_bus(0x079E, 0x30);
        }
        st.latch_and_advance_frame(inp, &mut Discard);
        if f < 600 || st.peek_bus(0x0770) != 1 || f % 4 != 0 {
            continue;
        }
        let cam = i32::from(st.peek_bus(0x071A)) * 256 + i32::from(st.peek_bus(0x071C));
        if !camera.contains(&cam) {
            continue;
        }
        let (right, left, inside) = census(&st);
        if right == 0
            || best.as_ref().is_some_and(|(c, _)| {
                (c.0 > 0) as usize + (c.1 > 0) as usize + (c.2 > 0) as usize
                    >= (right > 0) as usize + (left > 0) as usize + (inside > 0) as usize
            })
        {
            continue;
        }
        // Does the scene hold with the pad idle?
        let snap = st.snapshot().expect("snapshots");
        let mut later = EmuStepper::open(image).expect("opens");
        later.restore(&snap).expect("restores");
        for _ in 0..idle_frames {
            if unhurt_idle && later.peek_bus(0x0770) == 1 {
                later.poke_bus(0x079E, 0x30);
            }
            later.latch_and_advance_frame(input(0), &mut Discard);
        }
        let after = census(&later);
        if later.peek_bus(0x0770) == 1 && after.0 > 0 {
            if unhurt_idle {
                st.poke_bus(0x079E, 0xFF);
            }
            best = Some((
                (right, left, inside),
                st.save_state_with_mods(0, Some(session)).expect("saves"),
            ));
            if left > 0 && inside > 0 {
                break;
            }
        }
    }
    let (c, container) = best.expect("a scene with margin enemies");
    println!(
        "scene: enemies deep right {}, deep left {}, in the picture {}",
        c.0, c.1, c.2
    );
    container
}

pub fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

pub fn run_n(h: &mut Harness<'_, RetroForgeApp>, n: usize) {
    for _ in 0..n {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// The picture the app composed for the last frame.
pub fn grab(h: &Harness<'_, RetroForgeApp>) -> (usize, usize, Vec<u8>) {
    let rgba = h.state().last_frame_rgba_for_test().expect("a frame");
    let (w, hh) = h.state().frame_size_for_test().expect("a size");
    assert_eq!(rgba.len(), w * hh * 4);
    (w, hh, rgba)
}

pub fn save(path: &Path, w: usize, h: usize, rgba: &[u8]) {
    let mut opaque = rgba.to_vec();
    for px in opaque.chunks_exact_mut(4) {
        px[3] = 255;
    }
    image::save_buffer(path, &opaque, w as u32, h as u32, image::ColorType::Rgba8).expect("png");
    println!("SHOT {}", path.display());
}
