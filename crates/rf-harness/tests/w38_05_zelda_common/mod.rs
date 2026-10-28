//! Shared drive for the W38-05 Zelda tests (ROM-gated, law 5): the ROM loader
//! and the scripted play. Only offsets, counts and verdicts are printed (D-021).
//!
//! The script was found by watching the screen (menu flow, cave door position)
//! and the RAM that changed at the frames a screen scrolled; nothing is taken
//! from a community RAM map or a disassembly.
#![allow(dead_code)]

use std::io::Read as _;
use std::path::PathBuf;

use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};

pub const FILE: &str = "Legend of Zelda, The (USA).zip";
pub const A: u8 = 1 << 0;
pub const SELECT: u8 = 1 << 2;
pub const START: u8 = 1 << 3;
pub const UP: u8 = 1 << 4;
pub const DOWN: u8 = 1 << 5;
pub const LEFT: u8 = 1 << 6;
pub const RIGHT: u8 = 1 << 7;
/// First frame of play (the game start fade ends a little after it).
pub const PLAY_FROM: u32 = 720;
pub const FRAMES: u32 = 2800;

pub fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("nes").join(FILE);
    p.exists().then_some(p)
}

/// The single NES image inside the zip (content-sniffed, like the app).
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

/// Start (title), Start on "register your name", one letter, three Selects to
/// reach REGISTER, Start, then Start on the new file; then play: into the first
/// cave (Left, Up), out again (Down), Right across the overworld, then Up,
/// Left and Down so screens in every direction are crossed.
pub fn script(frame: u32) -> u8 {
    match frame {
        200..=204 | 300..=304 | 480..=484 | 600..=604 => START,
        340..=344 => A,
        400..=402 | 420..=422 | 440..=442 => SELECT,
        // In play.
        710..=753 => LEFT,
        754..=880 => UP,
        // In the cave: the text locks Link for a while; then right and up to
        // the item, a pause for the pickup, and back down and out.
        1261..=1270 => RIGHT,
        1271..=1330 => UP,
        1431..=1560 => DOWN,
        1561..=1633 => DOWN,
        1634..=2300 => RIGHT,
        2301..=2500 => UP,
        2501..=2540 => LEFT,
        2541..=2800 => DOWN,
        _ => 0,
    }
}

pub fn inputs(frames: u32) -> Vec<InputFrame> {
    (0..frames)
        .map(|f| {
            let mut i = InputFrame::empty();
            i.ports[0] = u16::from(script(f));
            i
        })
        .collect()
}

pub struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}
