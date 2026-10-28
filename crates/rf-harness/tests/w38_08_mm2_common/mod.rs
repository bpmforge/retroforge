//! Shared drive for the W38-08 Mega Man 2 tests (ROM-gated, law 5): the ROM
//! loader and the scripted play. Only offsets, counts and verdicts are printed
//! (D-021). The script was found by watching the screen (title, stage select).
#![allow(dead_code)]

use std::io::Read as _;
use std::path::PathBuf;

use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};

pub const FILE: &str = "Mega Man 2 (USA).zip";

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

/// Script text to one pad byte per frame: `wN` waits N frames, `<buttons>N`
/// holds the buttons (`A B E S U D L R`, joined by `+`) for N frames.
pub fn pads(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let (mask, n) = if let Some(n) = tok.strip_prefix('w') {
            (0u8, n.parse::<usize>().expect("wN"))
        } else {
            let split = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
            let (b, n) = tok.split_at(split);
            let mask = b.split('+').fold(0u8, |m, c| {
                m | match c {
                    "A" => 1,
                    "B" => 2,
                    "E" => 4,
                    "S" => 8,
                    "U" => 16,
                    "D" => 32,
                    "L" => 64,
                    "R" => 128,
                    other => panic!("button {other}"),
                }
            });
            (mask, n.parse().expect("N"))
        };
        out.extend(std::iter::repeat_n(mask, n));
    }
    out
}

pub fn inputs_of(pads: &[u8]) -> Vec<InputFrame> {
    pads.iter()
        .map(|&p| {
            let mut i = InputFrame::empty();
            i.ports[0] = u16::from(p);
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

/// Title, "Start/Password", stage select (the cursor starts on the boss at
/// the right of the middle row; one press right is not needed: Start picks
/// the highlighted portrait), the boss intro, then the stage's READY.
pub const MENU: &str = "w220 S4 w400 S4 w300 S4 w300 R6 w20 S4 w300 S4 w700";
/// First frame of play of a menu run (the stage's READY ends a little after).
pub const PLAY_FROM: u32 = 2266;

/// Health bar byte (found by a RAM scan of a run that took damage: the one
/// byte at 28 at full health, stepping down on each hit). Test-side only: the
/// scripted play holds it full so the player survives a long patrol; the poke
/// is applied identically to every run (never shipped).
pub const HEALTH: u16 = 0x06C0;

/// The forest stage: out to the right across the first stretch, back left and
/// out again (enemies are dropped and re-created on each pass), then down the
/// ladder shaft (vertical scroll) at the end of the stretch.
pub fn forest_script() -> Vec<u8> {
    let right = "R+B30 A+R+B25 ".repeat(14);
    let left = "L+B30 A+L+B25 ".repeat(14);
    let out = "R+B30 A+R+B25 ".repeat(25);
    let patrols = format!("{left}{right}").repeat(4);
    pads(&format!("{MENU} {out} {patrols} R40 D200 R20 D200 w50"))
}

/// The conveyor stage: the same menu with Down then Left to its portrait, then
/// right across the long stage and back, twice.
pub fn metal_script() -> Vec<u8> {
    let menu = "w220 S4 w400 S4 w300 S4 w300 D6 w20 L6 w20 S4 w300 S4 w700";
    let right = "R+B30 A+R+B25 ".repeat(40);
    let left = "L+B30 A+L+B25 ".repeat(20);
    let right2 = "R+B30 A+R+B25 ".repeat(20);
    pads(&format!("{menu} {right} {left} {right2} {left} {right2}"))
}

pub fn poke(f: usize, core: &mut rf_nes::core::NesCore) {
    use rf_nes::CpuBus as _;
    if f as u32 >= PLAY_FROM {
        core.bus_mut().write(HEALTH, 0x1C);
    }
}
