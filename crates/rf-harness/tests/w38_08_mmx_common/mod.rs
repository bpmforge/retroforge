//! Shared drive for the W38-08 Mega Man X tests (ROM-gated, law 5): the ROM
//! loader and the scripted play. Only offsets, counts and verdicts are printed
//! (D-021). The script was found by watching the screen (title, game start,
//! the opening highway stage).
#![allow(dead_code)]

use std::io::Read as _;
use std::path::PathBuf;

use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};
use rf_input::SnesButton;

pub const FILE: &str = "Mega Man X (USA).zip";
/// First frame of play (the stage's intro ends a little after it).
pub const PLAY_FROM: u32 = 1600;
pub const RAM_BASE: u32 = 0x7E_0000;

pub fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("snes").join(FILE);
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
            Ok(rf_cart::Cartridge::Snes { .. })
        ) {
            return buf;
        }
    }
    panic!("no SNES image in the zip");
}

/// Script text to one pad word per frame: `wN` waits, `<buttons>N` holds
/// (`A B X Y S E U D L R`, joined by `+`).
pub fn frames_of(text: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let (mask, n) = if let Some(n) = tok.strip_prefix('w') {
            (0u16, n.parse::<usize>().expect("wN"))
        } else {
            let split = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
            let (b, n) = tok.split_at(split);
            let mask = b.split('+').fold(0u16, |m, c| {
                let bit = match c {
                    "A" => SnesButton::A,
                    "B" => SnesButton::B,
                    "X" => SnesButton::X,
                    "Y" => SnesButton::Y,
                    "S" => SnesButton::Start,
                    "E" => SnesButton::Select,
                    "U" => SnesButton::Up,
                    "D" => SnesButton::Down,
                    "L" => SnesButton::Left,
                    "R" => SnesButton::Right,
                    other => panic!("button {other}"),
                }
                .bit();
                m | (1 << bit)
            });
            (mask, n.parse().expect("N"))
        };
        out.extend(std::iter::repeat_n(mask, n));
    }
    out
}

pub fn inputs_of(pads: &[u16]) -> Vec<InputFrame> {
    pads.iter()
        .map(|&p| {
            let mut i = InputFrame::empty();
            i.ports[0] = p;
            i
        })
        .collect()
}

/// Title ("GAME START"), Start, the stage's intro; play begins about frame 1500.
pub const MENU: &str = "w700 S6 w200 S6 w200 S6 w500";

/// The opening highway stage: right with shots and jumps, back left and out
/// again so enemies are dropped and re-created on each pass.
pub fn highway_script() -> Vec<u16> {
    let right = "R+Y30 B+R+Y25 ".repeat(30);
    let left = "L+Y30 B+L+Y25 ".repeat(20);
    let right2 = "R+Y30 B+R+Y25 ".repeat(20);
    frames_of(&format!(
        "{MENU} {right} {left} {right2} {left} {right2} {left} {right2}"
    ))
}

pub struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}
